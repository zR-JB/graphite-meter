package goclient

import (
	"errors"
	"fmt"
	"slices"
	"time"
)

const (
	minimumSurvivorEvidence = 800 * time.Millisecond
	minimumPeakWindow       = 500 * time.Millisecond
	maximumIntervals        = 128
)

type ReceiverSnapshot struct {
	ID           string
	Bytes, Nanos uint64
}

type ComponentWindow struct {
	ServerID    string
	Bytes       uint64
	Duration    time.Duration
	BytesPerSec float64
}

type AggregateWindow struct {
	Start, End                     time.Duration
	Down, Up                       []ComponentWindow
	DownBytesPerSec, UpBytesPerSec *float64
}

type AggregationInterval struct {
	Stage        Stage
	Participants []string
	Start, End   time.Duration
	Complete     bool
	Reason       string
	Window       *AggregateWindow
}

type FailureScope string

const (
	ScopeThroughput FailureScope = "throughput"
	ScopeLatency    FailureScope = "latency"
)

// ServerFailure records a server leaving the run; a throughput failure removes it, a latency one keeps it.
type ServerFailure struct {
	ServerID string
	Stage    Stage
	Scope    FailureScope
	Err      error
	At       time.Duration
}

type measurementBoundary struct {
	at         time.Duration
	down       map[string]uint64
	up         map[string]*ReceiverSnapshot
	observedUp map[string]uploadLedger
}

type byteLedger struct{ down, up uint64 }

func (b byteLedger) of(dir Direction) uint64 {
	if dir == Up {
		return b.up
	}
	return b.down
}

type uploadLedger struct {
	id      string
	maximum uint64
}

type componentKey struct {
	server string
	dir    Direction
}

type aggregateMeasurements struct {
	intervals             []AggregationInterval
	omitted               int
	first, last, peakFrom *measurementBoundary
	peaks                 map[Direction]float64
	samples               int
	serverPeaks           map[componentKey]float64
	serverSamples         map[string]int
	totals                map[string]byteLedger
	stageTotals           map[Stage]map[string]byteLedger
	uploads               map[string]uploadLedger
	downSeen              map[string]uint64
	stage                 Stage
	seen                  time.Duration
}

func (a *aggregateMeasurements) begin(stage Stage, ids []string, at time.Duration, reason string) {
	if a.totals == nil {
		a.totals = map[string]byteLedger{}
		a.stageTotals = map[Stage]map[string]byteLedger{}
	}
	if reason == "stage-start" {
		a.uploads = map[string]uploadLedger{}
		a.downSeen = map[string]uint64{}
	}
	a.stage = stage
	if a.stageTotals[stage] == nil {
		a.stageTotals[stage] = map[string]byteLedger{}
	}
	if len(a.intervals) == maximumIntervals {
		a.intervals = a.intervals[1:]
		a.omitted++
	}
	a.intervals = append(a.intervals, AggregationInterval{
		Stage:        stage,
		Participants: slices.Clone(ids),
		Start:        at,
		End:          at,
		Complete:     true,
		Reason:       reason,
	})
	a.first, a.last, a.peakFrom = nil, nil, nil
	a.peaks = map[Direction]float64{}
	a.samples = 0
	a.serverPeaks = map[componentKey]float64{}
	a.serverSamples = map[string]int{}
}
func (a *aggregateMeasurements) current() *AggregationInterval {
	if len(a.intervals) == 0 {
		return nil
	}
	return &a.intervals[len(a.intervals)-1]
}
func (a *aggregateMeasurements) credit(id string, dir Direction, n uint64) {
	total := a.totals[id]
	stage := a.stageTotals[a.stage][id]
	if dir == Down {
		total.down += n
		stage.down += n
	} else {
		total.up += n
		stage.up += n
	}
	a.totals[id] = total
	a.stageTotals[a.stage][id] = stage
}
func (a *aggregateMeasurements) ledger(boundary measurementBoundary) {
	for id, count := range boundary.down {
		if previous, known := a.downSeen[id]; known {
			if count < previous {
				continue
			}
			a.credit(id, Down, count-previous)
		}
		a.downSeen[id] = count
	}
	for id, observed := range boundary.observedUp {
		snapshot := boundary.up[id]
		if snapshot == nil || snapshot.ID != observed.id || snapshot.Bytes < observed.maximum {
			a.creditUpload(id, observed)
		}
	}
	for id, snapshot := range boundary.up {
		if snapshot != nil {
			a.creditUpload(id, uploadLedger{snapshot.ID, snapshot.Bytes})
		}
	}
}

func (a *aggregateMeasurements) creditUpload(id string, next uploadLedger) {
	previous, known := a.uploads[id]
	if known && previous.id == next.id && next.maximum <= previous.maximum {
		return
	}
	if known {
		if previous.id != next.id {
			previous.maximum = 0
		}
		a.credit(id, Up, next.maximum-previous.maximum)
	}
	a.uploads[id] = next
}

// observe returns the boundary's sample window, and whether the boundary restarted the interval.
func (a *aggregateMeasurements) observe(b measurementBoundary) (*AggregateWindow, bool) {
	interval := a.current()
	if interval == nil {
		return nil, false
	}
	a.ledger(b)
	if a.first != nil && b.at-a.seen > maximumBoundaryGap {
		return nil, a.restart(interval, b)
	}
	a.seen = b.at
	if len(interval.Participants) == 0 || slices.ContainsFunc(interval.Participants, func(id string) bool {
		_, down := b.down[id]
		return interval.Stage != StageUpload && !down || interval.Stage != StageDownload && b.up[id] == nil
	}) {
		return nil, false
	}
	if a.first == nil {
		a.first, a.last, a.peakFrom = new(b), new(b), new(b)
		interval.Start = b.at
		interval.End = b.at
		return nil, false
	}
	sample, err := aggregateWindow(*a.last, b, *interval)
	if errors.Is(err, errStaleBoundary) {
		return nil, false
	}
	full, fullErr := aggregateWindow(*a.first, b, *interval)
	if err != nil || fullErr != nil {
		return nil, a.restart(interval, b)
	}
	a.last = new(b)
	interval.End = b.at
	interval.Window = full
	a.samples++
	for _, id := range interval.Participants {
		a.serverSamples[id]++
	}
	if peak, err := aggregateWindow(*a.peakFrom, b, *interval); err == nil && peak.shortest() >= minimumPeakWindow {
		a.peakFrom = new(b)
		a.recordPeak(peak)
	}
	return sample, false
}

// shortest is the least span any clock covered; checkpoint retries can shrink a receiver's span.
func (w *AggregateWindow) shortest() time.Duration {
	span := w.End - w.Start
	for _, c := range w.Up {
		span = min(span, c.Duration)
	}
	return span
}

func (a *aggregateMeasurements) restart(interval *AggregationInterval, b measurementBoundary) bool {
	interval.Complete = false
	a.begin(interval.Stage, interval.Participants, b.at, "evidence-resumed")
	a.observe(b)
	return true
}

func (a *aggregateMeasurements) recordPeak(w *AggregateWindow) {
	for dir, rate := range map[Direction]*float64{Down: w.DownBytesPerSec, Up: w.UpBytesPerSec} {
		if rate != nil {
			a.peaks[dir] = max(a.peaks[dir], *rate)
		}
	}
	for dir, components := range map[Direction][]ComponentWindow{Down: w.Down, Up: w.Up} {
		for _, c := range components {
			key := componentKey{c.ServerID, dir}
			a.serverPeaks[key] = max(a.serverPeaks[key], c.BytesPerSec)
		}
	}
}

var errInsufficientEvidence = fmt.Errorf("the latest interval holds under %v of evidence", minimumSurvivorEvidence)

// errStaleBoundary marks a boundary without new clock evidence; it is skipped, never a zero rate.
var errStaleBoundary = errors.New("boundary did not advance")

func aggregateWindow(first, last measurementBoundary, interval AggregationInterval) (*AggregateWindow, error) {
	elapsed := last.at - first.at
	if elapsed <= 0 {
		return nil, errStaleBoundary
	}
	window := &AggregateWindow{Start: first.at, End: last.at}
	for _, id := range interval.Participants {
		if interval.Stage != StageUpload {
			start, ok := first.down[id]
			end, okEnd := last.down[id]
			if !ok || !okEnd || end < start {
				return nil, fmt.Errorf("missing or regressing download counter")
			}
			rate := float64(end-start) / elapsed.Seconds()
			window.Down = append(window.Down, ComponentWindow{id, end - start, elapsed, rate})
			if window.DownBytesPerSec == nil {
				window.DownBytesPerSec = new(float64)
			}
			*window.DownBytesPerSec += rate
		}
		if interval.Stage != StageDownload {
			start, end := first.up[id], last.up[id]
			if start.ID == end.ID && end.Bytes >= start.Bytes && end.Nanos == start.Nanos {
				return nil, errStaleBoundary
			}
			if start.ID != end.ID || end.Bytes < start.Bytes || end.Nanos < start.Nanos {
				return nil, fmt.Errorf("replaced or regressing receiver counter")
			}
			duration := time.Duration(end.Nanos - start.Nanos)
			rate := float64(end.Bytes-start.Bytes) / duration.Seconds()
			window.Up = append(window.Up, ComponentWindow{id, end.Bytes - start.Bytes, duration, rate})
			if window.UpBytesPerSec == nil {
				window.UpBytesPerSec = new(float64)
			}
			*window.UpBytesPerSec += rate
		}
	}
	return window, nil
}
func (a *aggregateMeasurements) result(stage Stage, dir Direction) Result {
	result := Result{Stage: stage, Direction: dir, Unavailable: true}
	for _, total := range a.stageTotals[stage] {
		result.TotalBytes += total.of(dir)
	}
	interval := a.current()
	if interval == nil ||
		interval.Stage != stage ||
		!interval.Complete ||
		interval.Window == nil ||
		interval.End-interval.Start < minimumSurvivorEvidence {
		result.Err = errInsufficientEvidence
		return result
	}
	components := interval.Window.Down
	rate := interval.Window.DownBytesPerSec
	if dir == Up {
		components = interval.Window.Up
		rate = interval.Window.UpBytesPerSec
	}
	if rate == nil ||
		len(components) == 0 ||
		slices.ContainsFunc(components, func(c ComponentWindow) bool { return c.Duration < minimumSurvivorEvidence }) {
		result.Err = errInsufficientEvidence
		return result
	}
	result.MeanBps = *rate
	result.PeakBps = a.peaks[dir]
	result.Samples = a.samples
	result.Elapsed = interval.End - interval.Start
	if dir == Up {
		result.Elapsed = 0
		for _, c := range components {
			result.Elapsed = max(result.Elapsed, c.Duration)
		}
	}
	result.Unavailable = false
	return result
}
