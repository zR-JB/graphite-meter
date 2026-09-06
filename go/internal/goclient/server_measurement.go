package goclient

import (
	"fmt"
	"slices"
	"time"
)

const minimumSurvivorEvidence = 800 * time.Millisecond
const maximumIntervals = 128

// ReceiverSnapshot retains one clock domain plus the client's request/response bracket.
type ReceiverSnapshot struct {
	ID                      string
	Bytes, Nanos            uint64
	RequestedAt, ReceivedAt time.Duration
}

type ComponentWindow struct {
	ServerID                   string
	Bytes                      uint64
	Duration                   time.Duration
	BytesPerSec                float64
	Clock                      string
	StartBytes, EndBytes       uint64
	StartReceiver, EndReceiver *ReceiverSnapshot
}

type AggregateWindow struct {
	Start, End                     time.Duration
	Down, Up                       []ComponentWindow
	DownBytesPerSec, UpBytesPerSec *float64
}

type AggregationInterval struct {
	ID           int
	Stage        string
	Participants []string
	Start, End   time.Duration
	Complete     bool
	Reason       string
	Window       *AggregateWindow
}

type ServerFailure struct {
	ServerID, Stage, Scope, Reason, Message string
	At                                      time.Duration
}

type measurementBoundary struct {
	at         time.Duration
	down       map[string]uint64
	up         map[string]*ReceiverSnapshot
	observedUp map[string]uploadLedger
}

type byteLedger struct{ down, up uint64 }
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
	stageTotals map[string]map[string]byteLedger
	uploads     map[string]uploadLedger
	downSeen    map[string]uint64
	stage       string
}

func (a *aggregateMeasurements) begin(stage string, ids []string, at time.Duration, reason string) {
	if a.totals == nil {
		a.totals = map[string]byteLedger{}
		a.stageTotals = map[string]map[string]byteLedger{}
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
	a.intervals = append(a.intervals, AggregationInterval{ID: a.omitted + len(a.intervals), Stage: stage, Participants: slices.Clone(ids), Start: at, End: at, Complete: true, Reason: reason})
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
		previous, known := a.downSeen[id]
		if known && count < previous {
			continue
		}
		if known && count >= previous {
			a.credit(id, Down, count-previous)
		}
		a.downSeen[id] = count
	}
	for id, observation := range boundary.observedUp {
		if snapshot := boundary.up[id]; snapshot != nil && snapshot.ID == observation.id && snapshot.Bytes >= observation.maximum {
			continue
		}
		previous, known := a.uploads[id]
		if known && previous.id == observation.id && observation.maximum <= previous.maximum {
			continue
		}
		if known {
			if previous.id != observation.id {
				previous.maximum = 0
			}
			if observation.maximum >= previous.maximum {
				a.credit(id, Up, observation.maximum-previous.maximum)
			}
		}
		a.uploads[id] = observation
	}
	for id, snapshot := range boundary.up {
		if snapshot == nil {
			continue
		}
		previous, known := a.uploads[id]
		if known && previous.id == snapshot.ID && snapshot.Bytes <= previous.maximum {
			continue
		}
		if known {
			if previous.id != snapshot.ID {
				previous.maximum = 0
			}
			if snapshot.Bytes >= previous.maximum {
				a.credit(id, Up, snapshot.Bytes-previous.maximum)
			}
		}
		a.uploads[id] = uploadLedger{snapshot.ID, snapshot.Bytes}
	}
}
func (a *aggregateMeasurements) observe(b measurementBoundary) *AggregateWindow {
	interval := a.current()
	if interval == nil {
		return nil
	}
	a.ledger(b)
	valid := len(interval.Participants) > 0
	for _, id := range interval.Participants {
		if interval.Stage != "upload" {
			_, ok := b.down[id]
			valid = valid && ok
		}
		if interval.Stage != "download" {
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
		if interval.Stage != "upload" {
			start, ok := first.down[id]
			end, okEnd := last.down[id]
			if !ok || !okEnd || end < start {
				return nil, fmt.Errorf("missing or regressing download counter")
			}
			rate := float64(end-start) / elapsed.Seconds()
			window.Down = append(window.Down, ComponentWindow{ServerID: id, Bytes: end - start, Duration: elapsed, BytesPerSec: rate, Clock: "client-monotonic", StartBytes: start, EndBytes: end})
			if window.DownBytesPerSec == nil {
				window.DownBytesPerSec = new(float64)
			}
			*window.DownBytesPerSec += rate
		}
		if interval.Stage != "download" {
			start, end := first.up[id], last.up[id]
			if start == nil || end == nil || start.ID != end.ID || end.Bytes < start.Bytes || end.Nanos <= start.Nanos {
				return nil, fmt.Errorf("missing or regressing receiver counter")
			}
			duration := time.Duration(end.Nanos - start.Nanos)
			if duration <= 0 {
				return nil, fmt.Errorf("invalid receiver duration")
			}
			rate := float64(end.Bytes-start.Bytes) / duration.Seconds()
			window.Up = append(window.Up, ComponentWindow{ServerID: id, Bytes: end.Bytes - start.Bytes, Duration: duration, BytesPerSec: rate, Clock: "receiver", StartBytes: start.Bytes, EndBytes: end.Bytes, StartReceiver: start, EndReceiver: end})
			if window.UpBytesPerSec == nil {
				window.UpBytesPerSec = new(float64)
			}
			*window.UpBytesPerSec += rate
		}
	}
	return window, nil
}
func (a *aggregateMeasurements) result(stage string, dir Direction) Result {
	result := Result{Stage: stage, Direction: dir, ServerAuth: dir == Up, Unavailable: true}
	for _, total := range a.stageTotals[stage] {
		if dir == Down {
			result.TotalBytes += total.down
		} else {
			result.TotalBytes += total.up
		}
	}
	interval := a.current()
	if interval == nil || interval.Stage != stage || !interval.Complete || interval.Window == nil || interval.End-interval.Start < minimumSurvivorEvidence {
		result.Err = fmt.Errorf("latest survivor interval has insufficient evidence")
		return result
	}
	components := interval.Window.Down
	rate := interval.Window.DownBytesPerSec
	if dir == Up {
		components = interval.Window.Up
		rate = interval.Window.UpBytesPerSec
	}
	if rate == nil || len(components) == 0 || slices.ContainsFunc(components, func(c ComponentWindow) bool { return c.Duration < minimumSurvivorEvidence }) {
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
