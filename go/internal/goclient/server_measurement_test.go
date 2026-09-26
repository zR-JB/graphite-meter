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

func TestCoordinatedReceiverWindows(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.beginStage("upload", []string{"a", "b"}, 0)
	a.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 100, 100),
		"b": nativeReceiver("b", 200, 100),
	}))
	sample, _ := a.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 1100, 1100),
		"b": nativeReceiver("b", 6200, 2100),
	}))
	if sample == nil || *sample.UpBytesPerSec != 4000 {
		t.Fatalf("sum of receiver-window means = %+v, want 4000 B/s", sample)
	}
	result := a.result(Up)
	if result.Unavailable || result.MeanBps != 4000 || result.TotalBytes != 7000 {
		t.Fatalf("result=%+v", result)
	}
	if sample.Up[0].Duration != time.Second || sample.Up[1].Duration != 2*time.Second {
		t.Fatalf("receiver durations were combined: %+v", sample.Up)
	}
	if result.Elapsed != 2*time.Second {
		t.Fatalf("receiver-timed elapsed = %v, want the receiver window", result.Elapsed)
	}
}

func TestPeaksNeedAMinimumWindow(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.beginStage("download", []string{"a", "b"}, 0)
	for i, bytes := range []uint64{0, 0, 1000, 1000, 2000, 2000, 3000} {
		a.observe(nativeBoundary(i*250, map[string]uint64{"a": bytes, "b": bytes / 2}, nil))
	}
	result := a.result(Down)
	if result.PeakBps != 3000 || result.MeanBps != 3000 || result.Samples != 6 {
		t.Fatalf("a burst inside a short window became the peak: %+v", result)
	}
	if own := a.current().servers; own["a"].peak.down != 2000 || own["b"].peak.down != 1000 || own["a"].samples != 6 {
		t.Fatalf("per-server peaks or samples = %+v %+v", own["a"], own["b"])
	}
}
func TestCoordinatedOppositeFluctuationsAndLedger(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.beginStage("download", []string{"a", "b"}, 0)
	a.observe(nativeBoundary(0, map[string]uint64{"a": 0, "b": 0}, nil))
	a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000, "b": 3000}, nil))
	a.observe(nativeBoundary(2000, map[string]uint64{"a": 4000, "b": 4000}, nil))
	result := a.result(Down)
	if result.MeanBps != 4000 || result.PeakBps != 4000 || result.TotalBytes != 8000 {
		t.Fatalf("independent peaks or durations leaked into aggregate: %+v", result)
	}
	a.restart([]string{"b"}, 2*time.Second, ReasonDropout)
	a.observe(nativeBoundary(2100, map[string]uint64{"b": 4200}, nil))
	a.observe(nativeBoundary(2500, map[string]uint64{"b": 5000}, nil))
	if result := a.result(Down); result.Unavailable || result.MeanBps != 4000 || result.PeakBps != 4000 ||
		result.TotalBytes != 9000 {
		t.Fatalf("a late dropout lost the headline of the interval before it: %+v", result)
	}
	if latest := a.current(); latest.combined.peak.down != 0 || latest.servers["b"].samples != 1 {
		t.Fatalf("peaks outlived their interval: %+v", latest)
	}
	if a.intervals[0].Window == nil || *a.intervals[0].Window.DownBytesPerSec != 4000 {
		t.Fatal("earlier evidence lost")
	}
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
func TestCoordinatedBidirectionalUsesCommonMembership(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.beginStage("bidirectional", []string{"a", "b"}, 0)
	a.observe(nativeBoundary(0, map[string]uint64{"a": 0, "b": 0}, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 0, 100),
		"b": nativeReceiver("b", 0, 100),
	}))
	a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000, "b": 2000}, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 1000, 1100),
		"b": nativeReceiver("b", 6000, 2100),
	}))
	if a.result(Down).MeanBps != 3000 || a.result(Up).MeanBps != 4000 {
		t.Fatal("bidirectional clocks were mixed")
	}
	a.restart(nil, time.Second, ReasonDropout)
	if !a.result(Down).Unavailable || !a.result(Up).Unavailable {
		t.Fatal("all failed must not retain the earlier headline")
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

func TestCoordinatedReceiverRegressionRevokesRateAndRetainsBytes(t *testing.T) {
	t.Parallel()
	var measurements aggregateMeasurements
	measurements.beginStage("upload", []string{"a"}, 0)
	measurements.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1000, 1000)}))
	measurements.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 3000, 3000)}))
	before := measurements.result(Up)
	if before.Unavailable || before.MeanBps != 1000 || before.TotalBytes != 2000 {
		t.Fatalf("receiver clock was replaced by the client clock: %+v", before)
	}
	measurements.observe(nativeBoundary(1500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 3000, 1500)}))
	after := measurements.result(Up)
	if !after.Unavailable || after.TotalBytes != before.TotalBytes || measurements.intervals[0].Window == nil {
		t.Fatalf("regressed receiver clock retained a rate or lost earlier bytes: %+v", after)
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

func TestMissingRequiredResultsAreNotComplete(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name    string
		replies int
		window  bool
		want    Outcome
	}{
		{"measured", 3, true, OutcomeComplete},
		{"no idle reply", 0, true, OutcomeIncomplete},
		{"no throughput window", 3, false, OutcomeIncomplete},
	} {
		cfg := DefaultConfig()
		cfg.Stages = StageSet{Latency: true, Download: true, Bidirectional: true}
		p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}}}
		p.results = []Result{{Stage: StageLatency, Latency: LatencyStats{Count: c.replies}}}
		co := &coordinator{cfg: cfg, servers: []*participant{p}, emit: func(Event) {}}
		for _, stage := range []StagePlan{{StageDownload, time.Second, []Direction{Down}},
			{StageBidirectional, time.Second, []Direction{Down, Up}}} {
			co.aggregate.beginStage(stage.Name, []string{"a"}, 0)
			if c.window {
				co.aggregate.observe(nativeBoundary(0, map[string]uint64{"a": 0},
					map[string]*ReceiverSnapshot{"a": nativeReceiver("u", 0, 0)}))
				co.aggregate.observe(nativeBoundary(1000, map[string]uint64{"a": 1000},
					map[string]*ReceiverSnapshot{"a": nativeReceiver("u", 1000, 1000)}))
			}
			(&stageRun{c: co, plan: stage}).finish(nil)
		}
		if got := co.outcome(t.Context(), nil); got != c.want {
			t.Errorf("%s: outcome %v, want %v", c.name, got, c.want)
		}
	}
}

func TestReceiverWindowsNeedOneAdvancingReceiver(t *testing.T) {
	t.Parallel()
	var a aggregateMeasurements
	a.beginStage(StageUpload, []string{"a"}, 0)
	first := nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1000, 1000)})
	for _, c := range []struct {
		name  string
		next  *ReceiverSnapshot
		stale bool
	}{
		{"replaced receiver", nativeReceiver("new", 5000, 2000), false},
		{"replaced receiver at the same clock", nativeReceiver("new", 5000, 1000), false},
		{"bytes without receiver time", nativeReceiver("id", 5000, 1000), true},
	} {
		window, err := a.window(first, nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": c.next}))
		if window != nil || err == nil || errors.Is(err, errStaleBoundary) != c.stale {
			t.Errorf("%s: window=%+v err=%v, want stale=%v", c.name, window, err, c.stale)
		}
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
		s.beginSampling(time.Now().Add(-redialWindow), measurementBoundary{down: map[string]uint64{"a": 100}})
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
		Peak     struct{ DownBytesPerSec, UpBytesPerSec any }
		Headline *struct{ Down, Up any }
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
				live = slices.DeleteFunc(slices.Clone(live), func(id string) bool { return slices.Contains(b.Dropout, id) })
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
		peak := func(dir Direction) any {
			if dir == Up && c.Stage == StageDownload || dir == Down && c.Stage == StageUpload {
				return nil
			}
			return a.current().combined.peak.of(dir)
		}
		if peak(Down) != c.Peak.DownBytesPerSec || peak(Up) != c.Peak.UpBytesPerSec {
			t.Errorf("%s: peak down=%v up=%v, want %+v", c.Name, peak(Down), peak(Up), c.Peak)
		}
		headline := func(dir Direction) any {
			if result := a.result(dir); !result.Unavailable {
				return result.MeanBps
			}
			return nil
		}
		want := struct{ Down, Up any }{}
		if c.Headline != nil {
			want = *c.Headline
		}
		if headline(Down) != want.Down || headline(Up) != want.Up {
			t.Errorf("%s: headline down=%v up=%v, want %+v", c.Name, headline(Down), headline(Up), c.Headline)
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
		s.beginSampling(time.Now(), initial)
		s.startSampler()
		defer func() { s.sampler.cancel(); s.sampling.Wait() }()
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

func TestAFinalBoundaryWithoutProgressKeepsTheLastGoodOne(t *testing.T) {
	t.Parallel()
	for _, moved := range []uint64{0, 100} {
		var a aggregateMeasurements
		a.beginStage(StageBidirectional, []string{"a"}, 0)
		up := map[string]*ReceiverSnapshot{"a": nativeReceiver("r", 0, 0)}
		a.observe(nativeBoundary(0, map[string]uint64{"a": 0}, up))
		a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000}, map[string]*ReceiverSnapshot{
			"a": nativeReceiver("r", 1000, 1000)}))
		final := nativeBoundary(1250, map[string]uint64{"a": 1250}, map[string]*ReceiverSnapshot{
			"a": nativeReceiver("r", 1000+moved, 1250)})
		final.final = true
		a.observe(final)
		want := 1250 * time.Millisecond
		if moved == 0 {
			want = time.Second
		}
		if end := a.current().End; end != want || a.result(Up).TotalBytes != 1000+moved {
			t.Errorf("moved %d: window ends at %v, want %v; %+v", moved, end, want, a.result(Up))
		}
	}
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
	s.beginSampling(time.Now(), initial)
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
