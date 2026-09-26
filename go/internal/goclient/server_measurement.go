package goclient

import (
	"cmp"
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

func (w *AggregateWindow) direction(dir Direction) ([]ComponentWindow, *float64) {
	if dir == Up {
		return w.Up, w.UpBytesPerSec
	}
	return w.Down, w.DownBytesPerSec
}

type IntervalReason string

const (
	ReasonStageStart      IntervalReason = "stage-start"
	ReasonDropout         IntervalReason = "dropout"
	ReasonEvidenceResumed IntervalReason = "evidence-resumed"
)

type AggregationInterval struct {
	Stage        Stage
	Participants []string
	Start, End   time.Duration
	Complete     bool
	Reason       IntervalReason
	Window       *AggregateWindow
}

type FailureScope string

const (
	ScopeThroughput FailureScope = "throughput"
	ScopeLatency    FailureScope = "latency"
)

type ServerFailure struct {
	ServerID string
	Stage    Stage
	Scope    FailureScope
	Reason   FailureReason
	Err      error
	At       time.Duration
}

type measurementBoundary struct {
	at         time.Duration
	down       map[string]uint64
	up         map[string]*ReceiverSnapshot
	observedUp map[string]uploadLedger
}

type uploadLedger struct {
	id      string
	maximum uint64
}

type serverLedger struct {
	down    *uint64
	upload  *uploadLedger
	bytes   byDirection[uint64]
	window  byDirection[*ComponentWindow]
	peak    byDirection[float64]
	samples int
}

type aggregateMeasurements struct {
	intervals             []AggregationInterval
	omitted               int
	stage                 Stage
	servers               map[string]*serverLedger
	first, last, peakFrom *measurementBoundary
	seen                  time.Duration
	peak                  byDirection[float64]
	samples               int
}

func (a *aggregateMeasurements) beginStage(stage Stage, ids []string, at time.Duration) {
	a.stage, a.servers = stage, map[string]*serverLedger{}
	for _, id := range ids {
		a.servers[id] = &serverLedger{}
	}
	a.restart(ids, at, ReasonStageStart)
}

func (a *aggregateMeasurements) restart(ids []string, at time.Duration, reason IntervalReason) {
	if len(a.intervals) == maximumIntervals {
		a.intervals = a.intervals[1:]
		a.omitted++
	}
	a.intervals = append(a.intervals, AggregationInterval{
		Stage:        a.stage,
		Participants: slices.Clone(ids),
		Start:        at,
		End:          at,
		Complete:     true,
		Reason:       reason,
	})
	a.first, a.last, a.peakFrom = nil, nil, nil
	a.peak, a.samples = byDirection[float64]{}, 0
	for _, server := range a.servers {
		server.peak, server.samples = byDirection[float64]{}, 0
	}
}

func (a *aggregateMeasurements) current() *AggregationInterval {
	if len(a.intervals) == 0 {
		return nil
	}
	return &a.intervals[len(a.intervals)-1]
}

func (a *aggregateMeasurements) total(dir Direction) uint64 {
	var total uint64
	for _, server := range a.servers {
		total += server.bytes.of(dir)
	}
	return total
}

func (a *aggregateMeasurements) credit(b measurementBoundary) {
	for id, count := range b.down {
		server := a.servers[id]
		switch {
		case server == nil:
		case server.down == nil:
			server.down = new(count)
		case count >= *server.down:
			server.bytes.down += count - *server.down
			server.down = new(count)
		}
	}
	for id, observed := range b.observedUp {
		if snapshot := b.up[id]; snapshot == nil || snapshot.ID != observed.id || snapshot.Bytes < observed.maximum {
			a.creditUpload(id, observed)
		}
	}
	for id, snapshot := range b.up {
		if snapshot != nil {
			a.creditUpload(id, uploadLedger{snapshot.ID, snapshot.Bytes})
		}
	}
}

func (a *aggregateMeasurements) creditUpload(id string, next uploadLedger) {
	server := a.servers[id]
	switch {
	case server == nil:
		return
	case server.upload == nil:
	case server.upload.id != next.id:
		server.bytes.up += next.maximum
	case next.maximum <= server.upload.maximum:
		return
	default:
		server.bytes.up += next.maximum - server.upload.maximum
	}
	server.upload = &next
}

func (a *aggregateMeasurements) observe(b measurementBoundary) (*AggregateWindow, bool) {
	interval := a.current()
	if interval == nil {
		return nil, false
	}
	a.credit(b)
	if a.first != nil && b.at-a.seen > maximumBoundaryGap {
		return nil, a.resume(interval, b)
	}
	a.seen = b.at
	if len(interval.Participants) == 0 || slices.ContainsFunc(interval.Participants, func(id string) bool {
		_, down := b.down[id]
		return a.stage != StageUpload && !down || a.stage != StageDownload && b.up[id] == nil
	}) {
		return nil, false
	}
	if a.first == nil {
		a.first, a.last, a.peakFrom = new(b), new(b), new(b)
		interval.Start, interval.End = b.at, b.at
		return nil, false
	}
	sample, err := a.window(*a.last, b)
	if errors.Is(err, errStaleBoundary) {
		return nil, false
	}
	full, fullErr := a.window(*a.first, b)
	if err != nil || fullErr != nil {
		return nil, a.resume(interval, b)
	}
	a.last = new(b)
	interval.End, interval.Window = b.at, full
	a.samples++
	for _, id := range interval.Participants {
		a.servers[id].samples++
	}
	for _, dir := range []Direction{Down, Up} {
		components, _ := full.direction(dir)
		for _, c := range components {
			a.servers[c.ServerID].window.set(dir, new(c))
		}
	}
	if peak, err := a.window(*a.peakFrom, b); err == nil && peak.shortest() >= minimumPeakWindow {
		a.peakFrom = new(b)
		a.recordPeak(peak)
	}
	return sample, false
}

func (w *AggregateWindow) shortest() time.Duration {
	span := w.End - w.Start
	for _, c := range w.Up {
		span = min(span, c.Duration)
	}
	return span
}

func (a *aggregateMeasurements) resume(interval *AggregationInterval, b measurementBoundary) bool {
	interval.Complete = false
	a.restart(interval.Participants, b.at, ReasonEvidenceResumed)
	a.observe(b)
	return true
}

func (a *aggregateMeasurements) recordPeak(w *AggregateWindow) {
	for _, dir := range []Direction{Down, Up} {
		components, rate := w.direction(dir)
		if rate != nil {
			a.peak.set(dir, max(a.peak.of(dir), *rate))
		}
		for _, c := range components {
			server := a.servers[c.ServerID]
			server.peak.set(dir, max(server.peak.of(dir), c.BytesPerSec))
		}
	}
}

var errInsufficientEvidence = fmt.Errorf("the latest interval holds under %v of evidence", minimumSurvivorEvidence)

var errStaleBoundary = errors.New("boundary did not advance")

func (a *aggregateMeasurements) window(first, last measurementBoundary) (*AggregateWindow, error) {
	elapsed := last.at - first.at
	if elapsed <= 0 {
		return nil, errStaleBoundary
	}
	window := &AggregateWindow{Start: first.at, End: last.at}
	for _, id := range a.current().Participants {
		if a.stage != StageUpload {
			start, ok := first.down[id]
			end, okEnd := last.down[id]
			if !ok || !okEnd || end < start {
				return nil, errors.New("missing or regressing download counter")
			}
			rate := float64(end-start) / elapsed.Seconds()
			window.Down = append(window.Down, ComponentWindow{id, end - start, elapsed, rate})
		}
		if a.stage != StageDownload {
			start, end := first.up[id], last.up[id]
			if start.ID == end.ID && end.Bytes >= start.Bytes && end.Nanos == start.Nanos {
				return nil, errStaleBoundary
			}
			if start.ID != end.ID || end.Bytes < start.Bytes || end.Nanos < start.Nanos {
				return nil, errors.New("replaced or regressing receiver counter")
			}
			duration := time.Duration(end.Nanos - start.Nanos)
			rate := float64(end.Bytes-start.Bytes) / duration.Seconds()
			window.Up = append(window.Up, ComponentWindow{id, end.Bytes - start.Bytes, duration, rate})
		}
	}
	window.DownBytesPerSec, window.UpBytesPerSec = sumRates(window.Down), sumRates(window.Up)
	return window, nil
}

func sumRates(components []ComponentWindow) *float64 {
	if len(components) == 0 {
		return nil
	}
	var sum float64
	for _, c := range components {
		sum += c.BytesPerSec
	}
	return &sum
}

func (a *aggregateMeasurements) result(dir Direction) Result {
	result := Result{Stage: a.stage, Direction: dir, Unavailable: true, Err: errInsufficientEvidence}
	result.TotalBytes = a.total(dir)
	interval := a.current()
	if interval == nil || !interval.Complete || interval.Window == nil ||
		interval.End-interval.Start < minimumSurvivorEvidence {
		return result
	}
	components, rate := interval.Window.direction(dir)
	short := func(c ComponentWindow) bool { return c.Duration < minimumSurvivorEvidence }
	if rate == nil || len(components) == 0 || slices.ContainsFunc(components, short) {
		return result
	}
	result.MeanBps, result.PeakBps, result.Samples = *rate, a.peak.of(dir), a.samples
	result.Elapsed = interval.End - interval.Start
	if dir == Up {
		result.Elapsed = slices.MaxFunc(components, func(x, y ComponentWindow) int {
			return cmp.Compare(x.Duration, y.Duration)
		}).Duration
	}
	result.Unavailable, result.Err = false, nil
	return result
}

func (a *aggregateMeasurements) serverResult(id string, dir Direction) Result {
	own := Result{Stage: a.stage, Direction: dir, Unavailable: true, Err: errInsufficientEvidence}
	server := a.servers[id]
	if server == nil {
		return own
	}
	own.TotalBytes = server.bytes.of(dir)
	if w := server.window.of(dir); w != nil && w.Duration >= minimumSurvivorEvidence {
		own.MeanBps, own.Elapsed = w.BytesPerSec, w.Duration
		own.PeakBps, own.Samples = server.peak.of(dir), server.samples
		own.Unavailable, own.Err = false, nil
	}
	return own
}
