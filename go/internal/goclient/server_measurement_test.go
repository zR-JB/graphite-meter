package goclient

import (
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"os"
	"reflect"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func nativeBoundary(ms int, down map[string]uint64, up map[string]*ReceiverSnapshot) measurementBoundary {
	return measurementBoundary{at: time.Duration(ms) * time.Millisecond, down: down, up: up}
}
func nativeReceiver(id string, bytes uint64, ms int) *ReceiverSnapshot {
	return &ReceiverSnapshot{ID: id, Bytes: bytes, Nanos: uint64(time.Duration(ms) * time.Millisecond)}
}

func TestCoordinatedZeroMissingAndRecovery(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.beginStage("upload", []string{"a"}, 0)
	a.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 100, 100)}))
	a.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 100, 1100)}))
	if result := a.result(Up); !result.Unavailable || !errors.Is(result.Err, errNoBytes) {
		t.Fatalf("a window that moved nothing kept a headline: %+v", result)
	}
	missing := nativeBoundary(1200, nil, map[string]*ReceiverSnapshot{"a": nil})
	missing.observedUp = map[string]uploadLedger{"a": {"id", 500}}
	a.observe(missing)
	a.observe(nativeBoundary(1300, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 500, 1100)}))
	if result := a.result(Up); !result.Unavailable || result.TotalBytes != 400 || a.intervals[0].End != time.Second ||
		len(a.intervals) != 1 {
		t.Fatalf("a missing or stale checkpoint must be skipped, keeping bytes and the window: %+v", result)
	}
	a.observe(nativeBoundary(1500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 700, 1600)}))
	a.observe(nativeBoundary(2500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1700, 2600)}))
	if result := a.result(Up); result.Unavailable || result.MeanBps != 640 || result.TotalBytes != 1600 ||
		len(a.intervals) != 1 {
		t.Fatalf("the next valid boundary must span the gap in the receiver clock: %+v", result)
	}
	a.observe(nativeBoundary(3500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("new", 100, 100)}))
	if len(a.intervals) != 2 || a.intervals[1].Reason != "evidence-resumed" ||
		a.result(Up).TotalBytes != 1700 {
		t.Fatalf("a replaced receiver must start a fresh interval: %+v", a.intervals)
	}
}
func TestCoordinatedIntervalsStayBounded(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	for i := range 140 {
		a.beginStage("download", []string{"a"}, time.Duration(i)*time.Second)
		a.observe(nativeBoundary(i*1000, map[string]uint64{"a": 0}, nil))
		a.observe(nativeBoundary((i+1)*1000, map[string]uint64{"a": 1000}, nil))
	}
	if len(a.intervals) != maximumIntervals || a.omitted != 12 || math.IsNaN(a.result(Down).MeanBps) {
		t.Fatalf("bounds=%d omitted=%d", len(a.intervals), a.omitted)
	}
}

func TestCheckpointMissesRemoveServers(t *testing.T) {
	t.Parallel()
	s := &stageRun{misses: map[string]int{}}
	refused := errors.New("refused")
	for i, final := range []bool{false, false, true} {
		if s.dropsServer("a", refused, final) {
			t.Fatalf("miss %d removed the server", i+1)
		}
	}
	if s.dropsServer("a", nil, false) || s.dropsServer("a", refused, false) || s.dropsServer("a", refused, false) {
		t.Fatal("a successful checkpoint did not reset the count")
	}
	if !s.dropsServer("a", refused, false) {
		t.Fatal("the third consecutive miss kept the server")
	}
	if !s.dropsServer("b", &AuthRequiredError{}, true) {
		t.Fatal("a refused grant kept the server")
	}
}

func TestSilentDirectionsLeaveAfterTheRedialWindow(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name    string
		moved   uint64
		removed bool
	}{{"silent", 0, true}, {"one byte", 1, false}} {
		server := PreparedServer{Server: wire.ServerEntry{ID: "a"}, Connection: &PreparedConnection{}}
		p := &participant{prepared: server}
		co := &coordinator{servers: []*participant{p}, emit: func(Event) {}}
		stage := StagePlan{Name: StageDownload, Directions: []Direction{Down}}
		co.aggregate.beginStage(stage.Name, []string{"a"}, 0)
		own := &stageServer{participant: p, cancelTransfer: func(error) {}, cancelLatency: func(error) {}}
		s := &stageRun{c: co, plan: stage, servers: []*stageServer{own}}
		co.aggregate.observe(nativeBoundary(0, map[string]uint64{"a": 100}, nil))
		s.beginSampling(time.Now().Add(-redialWindow))
		s.observe(sampledBoundary{boundary: nativeBoundary(1000, map[string]uint64{"a": 100 + c.moved}, nil)})
		if p.removed != c.removed {
			t.Errorf("%s: removed = %v, want %v", c.name, p.removed, c.removed)
		}
	}
}

func TestAggregationMatchesTheSharedVectors(t *testing.T) {
	t.Parallel()
	data, err := os.ReadFile("../../../api/aggregation.testvectors.json")
	if err != nil {
		t.Fatal(err)
	}
	type window struct {
		StartMs, EndMs                 int64
		DownBytesPerSec, UpBytesPerSec *float64
	}
	type reported struct{ BytesPerSec, PeakBytesPerSec float64 }
	type results struct{ Down, Up *reported }
	var cases []struct {
		Name         string
		Stage        Stage
		Participants []string
		Boundaries   []struct {
			AtMs    int64
			Final   bool
			Dropout []string
			Down    map[string]uint64
			Up      map[string]*struct {
				ID           string
				Bytes, Nanos uint64
			}
		}
		Intervals []struct {
			Reason   IntervalReason
			Complete bool
			Window   *window
		}
		Result results
	}
	if err := json.Unmarshal(data, &cases, json.MatchCaseInsensitiveNames(true)); err != nil {
		t.Fatal(err)
	}
	for _, c := range cases {
		var a aggregateMeasurements
		a.beginStage(c.Stage, c.Participants, time.Duration(c.Boundaries[0].AtMs)*time.Millisecond)
		live := c.Participants
		for _, b := range c.Boundaries {
			if len(b.Dropout) > 0 {
				dropped := func(id string) bool { return slices.Contains(b.Dropout, id) }
				live = slices.DeleteFunc(slices.Clone(live), dropped)
				a.restart(live, time.Duration(b.AtMs)*time.Millisecond, ReasonDropout)
			}
			boundary := nativeBoundary(int(b.AtMs), b.Down, map[string]*ReceiverSnapshot{})
			boundary.final = b.Final
			for id, r := range b.Up {
				if r != nil {
					boundary.up[id] = &ReceiverSnapshot{ID: r.ID, Bytes: r.Bytes, Nanos: r.Nanos}
				}
			}
			a.observe(boundary)
		}
		reported := func(dir Direction) *reported {
			if result := a.result(dir); !result.Unavailable {
				return &reported{result.MeanBps, result.PeakBps}
			}
			return nil
		}
		if got := (results{reported(Down), reported(Up)}); !reflect.DeepEqual(got, c.Result) {
			t.Errorf("%s: result down=%+v up=%+v, want down=%+v up=%+v", c.Name, got.Down, got.Up,
				c.Result.Down, c.Result.Up)
		}
		if len(a.intervals) != len(c.Intervals) {
			t.Errorf("%s: %d intervals, want %d", c.Name, len(a.intervals), len(c.Intervals))
			continue
		}
		for i, want := range c.Intervals {
			got := a.intervals[i]
			var w *window
			if got.Window != nil {
				w = &window{got.Window.Start.Milliseconds(), got.Window.End.Milliseconds(),
					got.Window.DownBytesPerSec, got.Window.UpBytesPerSec}
			}
			if got.Reason != want.Reason || got.Complete != want.Complete || !reflect.DeepEqual(w, want.Window) {
				t.Errorf("%s interval %d: %s complete=%v window=%+v, want %+v", c.Name, i, got.Reason, got.Complete,
					w, want)
			}
		}
	}
}

func TestOnlyALateTickResumesEvidence(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		begun, slow := time.Now(), atomic.Bool{}
		transport := roundTripFunc(func(req *http.Request) (*http.Response, error) {
			if slow.Swap(false) {
				time.Sleep(1400 * time.Millisecond)
			}
			nanos := time.Since(begun).Nanoseconds() + 1
			body := fmt.Sprintf(`{"bytes":%d,"nanos":%d}`, nanos/1000, nanos)
			return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(body)),
				Request: req}, nil
		})
		r := &runner{http: &http.Client{Transport: transport}, target: fetchTarget("http://meter.test"),
			coordinated: &participantCounters{}}
		r.coordinated.upload.Store(newUploadProgress(t.Context(), "u"))
		p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}}, transport: r}
		co := &coordinator{servers: []*participant{p}, started: begun, emit: func(Event) {}}
		own := []*stageServer{{participant: p, cancelTransfer: func(error) {}, cancelLatency: func(error) {}}}
		s := &stageRun{c: co, plan: StagePlan{Name: StageUpload, Directions: []Direction{Up}}, ctx: t.Context(),
			servers: own}
		initial, _ := s.collect(t.Context(), co.active(), checkpointBudget)
		co.aggregate.beginStage(StageUpload, []string{"a"}, initial.at)
		co.aggregate.observe(initial)
		s.beginSampling(time.Now())
		s.startSampler()
		defer func() {
			s.sampler.cancel()
			s.sampling.Wait()
		}()
		observe := func(n int) {
			for range n {
				s.observe(<-s.results())
			}
		}
		observe(3)
		slow.Store(true)
		observe(4)
		if len(co.aggregate.intervals) != 1 {
			t.Fatalf("a slow checkpoint resumed evidence: %+v", co.aggregate.intervals)
		}
		time.Sleep(3 * time.Second)
		observe(2)
		if len(co.aggregate.intervals) != 2 || co.aggregate.intervals[1].Reason != ReasonEvidenceResumed {
			t.Fatalf("a stalled client kept one window: %+v", co.aggregate.intervals)
		}
	})
}

func TestLiveRatesRestartOnlyWithTheInterval(t *testing.T) {
	t.Parallel()
	p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}}}
	var live []ThroughputSample
	co := &coordinator{servers: []*participant{p}, emit: func(e Event) { live = append(live, e.Throughput) }}
	stage := StagePlan{Name: StageUpload, Directions: []Direction{Up}}
	initial := nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("r1", 0, 1000)})
	co.aggregate.beginStage(stage.Name, []string{"a"}, 0)
	co.aggregate.observe(initial)
	own := []*stageServer{{participant: p, cancelTransfer: func(error) {}, cancelLatency: func(error) {}}}
	s := &stageRun{c: co, plan: stage, servers: own}
	s.beginSampling(time.Now())
	for i, step := range []struct {
		receiver *ReceiverSnapshot
		want     []ThroughputSample
	}{
		{nativeReceiver("r1", 1000, 2000), []ThroughputSample{{BytesPerSec: 1000, TotalBytes: 1000}}},
		{nil, nil},
		{nativeReceiver("r1", 1500, 2000), nil},
		{nativeReceiver("r2", 100, 100), []ThroughputSample{{Unavailable: true}}},
	} {
		live = nil
		up := map[string]*ReceiverSnapshot{"a": step.receiver}
		sample := sampledBoundary{boundary: nativeBoundary(1000*(i+1), nil, up)}
		if step.receiver == nil {
			sample.misses = map[string]error{"a": errors.New("missed")}
		}
		s.observe(sample)
		if !reflect.DeepEqual(live, step.want) {
			t.Errorf("step %d: live rates %+v, want %+v", i, live, step.want)
		}
	}
}
