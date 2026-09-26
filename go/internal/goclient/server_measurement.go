package goclient

import (
	"fmt"
	"slices"
	"time"
)

const minimumSurvivorEvidence = 800 * time.Millisecond
const maximumIntervals = 128

type ReceiverSnapshot struct {
	ID           string
	Bytes, Nanos uint64
}

// ComponentWindow is one server's share; upload durations come from the receiver clock.
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
	ID           int
	Stage        Stage
	Participants []string
	Start, End   time.Duration
	Complete     bool
	Reason       string
	Window       *AggregateWindow
}

type ServerFailure struct {
	ServerID               string
	Stage                  Stage
	Scope, Reason, Message string
	At                     time.Duration
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

// aggregateMeasurements is single-owner: it never adds receiver durations or independent peaks.
type aggregateMeasurements struct {
	intervals   []AggregationInterval
	omitted     int
	first, last *measurementBoundary
	peaks       map[Direction]float64
	samples     int
	totals      map[string]byteLedger
	stageTotals map[Stage]map[string]byteLedger
	uploads     map[string]uploadLedger
	downSeen    map[string]uint64
	stage       Stage
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
		ID:           a.omitted + len(a.intervals),
		Stage:        stage,
		Participants: slices.Clone(ids),
		Start:        at,
		End:          at,
		Complete:     true,
		Reason:       reason,
	})
	a.first = nil
	a.last = nil
	a.peaks = map[Direction]float64{}
	a.samples = 0
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

func (a *aggregateMeasurements) observe(b measurementBoundary) *AggregateWindow {
	interval := a.current()
	if interval == nil {
		return nil
	}
	a.ledger(b)
	valid := len(interval.Participants) > 0
	for _, id := range interval.Participants {
		if interval.Stage != StageUpload {
			_, ok := b.down[id]
			valid = valid && ok
		}
		if interval.Stage != StageDownload {
			valid = valid && b.up[id] != nil
		}
	}
	if !valid {
		interval.Complete = false
		interval.End = b.at
		return nil
	}
	if !interval.Complete {
		a.begin(interval.Stage, interval.Participants, b.at, "evidence-resumed")
		return a.observe(b)
	}
	if a.first == nil {
		a.first = new(b)
		a.last = new(b)
		interval.Start = b.at
		interval.End = b.at
		return nil
	}
	sample, err := aggregateWindow(*a.last, b, *interval)
	full, fullErr := aggregateWindow(*a.first, b, *interval)
	if err != nil || fullErr != nil {
		interval.Complete = false
		interval.End = b.at
		a.begin(interval.Stage, interval.Participants, b.at, "evidence-resumed")
		return a.observe(b)
	}
	a.last = new(b)
	interval.End = b.at
	interval.Window = full
	a.samples++
	if sample.DownBytesPerSec != nil {
		a.peaks[Down] = max(a.peaks[Down], *sample.DownBytesPerSec)
	}
	if sample.UpBytesPerSec != nil {
		a.peaks[Up] = max(a.peaks[Up], *sample.UpBytesPerSec)
	}
	return sample
}
func aggregateWindow(first, last measurementBoundary, interval AggregationInterval) (*AggregateWindow, error) {
	elapsed := last.at - first.at
	if elapsed <= 0 {
		return nil, fmt.Errorf("non-advancing client boundary")
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
			if start == nil || end == nil || start.ID != end.ID || end.Bytes < start.Bytes || end.Nanos <= start.Nanos {
				return nil, fmt.Errorf("missing or regressing receiver counter")
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
		result.Err = fmt.Errorf("latest survivor interval has insufficient evidence")
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
		result.Err = fmt.Errorf("latest receiver windows have insufficient evidence")
		return result
	}
	result.MeanBps = *rate
	result.PeakBps = a.peaks[dir]
	result.Samples = a.samples
	result.Elapsed = interval.End - interval.Start
	result.Unavailable = false
	return result
}
