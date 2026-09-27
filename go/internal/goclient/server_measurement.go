package goclient

import (
	"cmp"
	"errors"
	"fmt"
	"slices"
	"time"
)

const maximumIntervals = 128

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
	combined     windowStats
	servers      map[string]*windowStats
	moved        map[string]*byDirection[*intervalMark]
}

type intervalMark struct {
	boundary measurementBoundary
	window   *AggregateWindow
	combined windowStats
	servers  map[string]windowStats
}

type windowStats struct {
	peak    byDirection[float64]
	samples int
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
	stalled    bool
	final      bool
	down       map[string]uint64
	up         map[string]*ReceiverSnapshot
	observedUp map[string]uploadLedger
}

type uploadLedger struct {
	id      string
	maximum uint64
}

type serverLedger struct {
	down   *uint64
	upload *uploadLedger
	bytes  byDirection[uint64]
}

type aggregateMeasurements struct {
	intervals             []AggregationInterval
	omitted               int
	stage                 Stage
	servers               map[string]*serverLedger
	first, last, peakFrom *measurementBoundary
	latest                *measurementBoundary
}

func (a *aggregateMeasurements) beginStage(stage Stage, ids []string, at time.Duration) {
	a.stage, a.servers, a.latest = stage, map[string]*serverLedger{}, nil
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
	interval := AggregationInterval{Stage: a.stage, Participants: slices.Clone(ids), Start: at, End: at,
		Complete: true, Reason: reason, servers: map[string]*windowStats{},
		moved: map[string]*byDirection[*intervalMark]{}}
	for _, id := range ids {
		interval.servers[id] = &windowStats{}
		interval.moved[id] = &byDirection[*intervalMark]{}
	}
	a.intervals = append(a.intervals, interval)
	a.first, a.last, a.peakFrom = nil, nil, nil
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
	a.latest = new(b)
	if a.first != nil && b.stalled {
		return nil, a.resume(interval, b)
	}
	if len(interval.Participants) == 0 || slices.ContainsFunc(interval.Participants, func(id string) bool {
		_, down := b.down[id]
		return a.stage != StageUpload && !down || a.stage != StageDownload && b.up[id] == nil
	}) {
		return nil, false
	}
	if a.first == nil {
		a.start(interval, b)
		return nil, false
	}
	// A final boundary where a direction stood still ends the result at the last good boundary.
	if b.final && slices.ContainsFunc(interval.Participants, func(id string) bool {
		return a.stage != StageUpload && b.down[id] <= a.last.down[id] ||
			a.stage != StageDownload && b.up[id].ID == a.last.up[id].ID && b.up[id].Bytes <= a.last.up[id].Bytes
	}) {
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
	interval.combined.samples++
	for _, id := range interval.Participants {
		interval.servers[id].samples++
	}
	if peak, err := a.window(*a.peakFrom, b); err == nil && peak.shortest() >= minimumPeakWindow {
		a.peakFrom = new(b)
		interval.recordPeak(peak)
	}
	mark := interval.mark(b)
	for _, dir := range []Direction{Down, Up} {
		components, _ := sample.direction(dir)
		for _, c := range components {
			if c.Bytes > 0 {
				interval.moved[c.ServerID].set(dir, mark)
			}
		}
	}
	return sample, false
}

func (a *aggregateMeasurements) start(interval *AggregationInterval, b measurementBoundary) {
	a.first, a.last, a.peakFrom = new(b), new(b), new(b)
	interval.Start, interval.End = b.at, b.at
	start := interval.mark(b)
	for _, moved := range interval.moved {
		if a.stage != StageUpload {
			moved.down = start
		}
		if a.stage != StageDownload {
			moved.up = start
		}
	}
}

func (interval *AggregationInterval) mark(b measurementBoundary) *intervalMark {
	m := &intervalMark{boundary: b, window: interval.Window, combined: interval.combined,
		servers: map[string]windowStats{}}
	for id, stats := range interval.servers {
		m.servers[id] = *stats
	}
	return m
}

func (a *aggregateMeasurements) dropout(survivors []string, at time.Duration) {
	interval := a.current()
	if interval == nil || len(survivors) == len(interval.Participants) {
		return
	}
	var end *intervalMark
	for id, moved := range interval.moved {
		if slices.Contains(survivors, id) {
			continue
		}
		for _, m := range []*intervalMark{moved.down, moved.up} {
			if m != nil && (end == nil || m.boundary.at < end.boundary.at) {
				end = m
			}
		}
	}
	if end == nil {
		if len(survivors) > 0 {
			a.restart(survivors, at, ReasonDropout)
		}
		return
	}
	interval.End, interval.Window, interval.combined = end.boundary.at, end.window, end.combined
	for id, stats := range end.servers {
		*interval.servers[id] = stats
	}
	if len(survivors) == 0 {
		return
	}
	latest := a.latest
	a.restart(survivors, end.boundary.at, ReasonDropout)
	a.start(a.current(), end.boundary)
	if latest.at > end.boundary.at {
		a.observe(*latest)
	}
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

func (interval *AggregationInterval) recordPeak(w *AggregateWindow) {
	for _, dir := range []Direction{Down, Up} {
		components, rate := w.direction(dir)
		if rate != nil {
			interval.combined.peak.set(dir, max(interval.combined.peak.of(dir), *rate))
		}
		for _, c := range components {
			server := interval.servers[c.ServerID]
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
	stale := false
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
			if start.ID != end.ID || end.Bytes < start.Bytes || end.Nanos < start.Nanos {
				return nil, errors.New("replaced or regressing receiver counter")
			}
			if end.Nanos == start.Nanos {
				stale = true
				continue
			}
			duration := time.Duration(end.Nanos - start.Nanos)
			rate := float64(end.Bytes-start.Bytes) / duration.Seconds()
			window.Up = append(window.Up, ComponentWindow{id, end.Bytes - start.Bytes, duration, rate})
		}
	}
	if stale {
		return nil, errStaleBoundary
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

func evidence(components []ComponentWindow) error {
	short := func(c ComponentWindow) bool { return c.Duration < minimumSurvivorEvidence }
	switch {
	case len(components) == 0 || slices.ContainsFunc(components, short):
		return errInsufficientEvidence
	case !slices.ContainsFunc(components, func(c ComponentWindow) bool { return c.Bytes > 0 }):
		return errNoBytes
	}
	return nil
}

// result takes the latest whole interval with enough evidence, so a late dropout keeps the one before it.
func (a *aggregateMeasurements) result(dir Direction) Result {
	result := Result{Stage: a.stage, Direction: dir, Unavailable: true, Err: errInsufficientEvidence}
	result.TotalBytes = a.total(dir)
	if a.current() == nil {
		return result
	}
	for i := len(a.intervals) - 1; i >= 0 && a.intervals[i].Stage == a.stage; i-- {
		interval := &a.intervals[i]
		if interval.Window == nil || !interval.Complete {
			continue
		}
		components, rate := interval.Window.direction(dir)
		err := evidence(components)
		if interval.End-interval.Start < minimumSurvivorEvidence {
			err = errInsufficientEvidence
		}
		if err != nil {
			if i == len(a.intervals)-1 {
				result.Err = err
			}
			continue
		}
		stats := interval.combined
		result.MeanBps, result.PeakBps, result.Samples = *rate, max(stats.peak.of(dir), *rate), stats.samples
		result.Elapsed = interval.End - interval.Start
		if dir == Up {
			result.Elapsed = slices.MaxFunc(components, func(x, y ComponentWindow) int {
				return cmp.Compare(x.Duration, y.Duration)
			}).Duration
		}
		result.Unavailable, result.Err = false, nil
		return result
	}
	return result
}

func (a *aggregateMeasurements) serverResult(id string, dir Direction) Result {
	own := Result{Stage: a.stage, Direction: dir, Unavailable: true, Err: errInsufficientEvidence}
	if server := a.servers[id]; server != nil {
		own.TotalBytes = server.bytes.of(dir)
	}
	for i := len(a.intervals) - 1; i >= 0 && a.intervals[i].Stage == a.stage; i-- {
		interval := &a.intervals[i]
		if interval.Window == nil || !interval.Complete {
			continue
		}
		components, _ := interval.Window.direction(dir)
		j := slices.IndexFunc(components, func(c ComponentWindow) bool { return c.ServerID == id })
		if j < 0 || evidence(components[j:j+1]) != nil {
			continue
		}
		stats := interval.servers[id]
		own.MeanBps, own.Elapsed = components[j].BytesPerSec, components[j].Duration
		own.PeakBps, own.Samples = max(stats.peak.of(dir), own.MeanBps), stats.samples
		own.Unavailable, own.Err = false, nil
		return own
	}
	return own
}
