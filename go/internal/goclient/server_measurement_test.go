package goclient

import (
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"reflect"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func nativeBoundary(ms int, down map[string]uint64, up map[string]*ReceiverSnapshot) measurementBoundary {
	return measurementBoundary{at: time.Duration(ms) * time.Millisecond, down: down, up: up}
}

func testStage(p *participant, plan StagePlan, emit func(Event), others ...*participant) *stageRun {
	c := &coordinator{prepared: &PreparedRun{}, servers: append([]*participant{p}, others...), started: time.Now(),
		emit: emit}
	c.aggregate.beginStage(plan.Name, c.ids(), 0)
	s := &stageRun{c: c, plan: plan}
	for _, p := range c.servers {
		s.servers = append(s.servers,
			&stageServer{participant: p, cancelTransfer: func(error) {}, cancelLatency: func(error) {}})
	}
	return s
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

func TestCheckpointMissesRemoveServersOnlyWhileAnotherMoves(t *testing.T) {
	t.Parallel()
	a := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}}}
	b := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "b"}}}
	s := testStage(a, StagePlan{Name: StageUpload, Directions: []Direction{Up}}, func(Event) {}, b)
	now := time.Now()
	s.beginSampling(now)
	own, other := s.servers[0], s.servers[1]
	refused := errors.New("refused")
	for i, final := range []bool{false, false, true} {
		if s.dropsServer(own, refused, final, now) {
			t.Fatalf("miss %d removed the server", i+1)
		}
	}
	if s.dropsServer(own, nil, false, now) || s.dropsServer(own, refused, false, now) ||
		s.dropsServer(own, refused, false, now) {
		t.Fatal("a successful checkpoint did not reset the count")
	}
	if !s.dropsServer(own, refused, false, now) {
		t.Fatal("the third consecutive miss kept the server while another moved")
	}
	s.lastMovement["b"].set(Up, now.Add(-redialWindow))
	if s.dropsServer(own, refused, false, now) {
		t.Fatal("misses while no server moves removed the server")
	}
	if !s.dropsServer(other, &AuthRequiredError{}, true, now) {
		t.Fatal("a refused grant kept the server")
	}
}

func TestSilenceRemovesAServerOnlyWhileAnotherMoves(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name    string
		moved   []uint64 // bytes servers a and b move over the redial window
		final   bool
		removed []bool
	}{
		{"a sole silent server stays", []uint64{0}, false, []bool{false}},
		{"a byte keeps it", []uint64{1}, false, []bool{false}},
		{"the stage end removes it", []uint64{0}, true, []bool{true}},
		{"silence both servers share removes neither", []uint64{0, 0}, false, []bool{false, false}},
		{"silence beside a moving server removes it", []uint64{0, 1}, false, []bool{true, false}},
	} {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			var servers []*participant
			start, end := map[string]uint64{}, map[string]uint64{}
			for i, moved := range c.moved {
				id := string(rune('a' + i))
				servers = append(servers, &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: id}},
					transport: &runner{coordinated: &participantCounters{}}})
				start[id], end[id] = 100, 100+moved
			}
			s := testStage(servers[0], StagePlan{Name: StageDownload, Directions: []Direction{Down}}, func(Event) {},
				servers[1:]...)
			s.ctx, s.sampler = t.Context(), &sampler{cancel: func() {}}
			s.c.aggregate.observe(nativeBoundary(0, start, nil))
			s.beginSampling(time.Now().Add(-redialWindow))
			boundary := nativeBoundary(1000, end, nil)
			boundary.final, s.ended = c.final, time.Now()
			s.observe(sampledBoundary{boundary: boundary})
			s.sampler.cancel()
			s.sampling.Wait()
			for i, p := range servers {
				if p.removed != c.removed[i] {
					t.Errorf("%s removed = %v, want %v", p.id(), p.removed, c.removed[i])
				}
			}
		})
	}
}

// A run's last moments: evidence that moved shortly before the window closed keeps its server, however late the final
// checkpoint answers and whatever its transfer reports while shutting down.
func TestAServerMovingAsTheWindowClosesKeepsItsStage(t *testing.T) {
	t.Parallel()
	stage := func(sinceMovement time.Duration) (*stageRun, *participant) {
		p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}},
			transport: &runner{coordinated: &participantCounters{}}}
		s := testStage(p, StagePlan{Name: StageDownload, Directions: []Direction{Down}}, func(Event) {})
		s.ctx, s.sampler = t.Context(), &sampler{cancel: func() {}}
		s.c.aggregate.observe(nativeBoundary(0, map[string]uint64{"a": 100}, nil))
		s.ending, s.ended = true, time.Now()
		s.beginSampling(s.ended.Add(-sinceMovement))
		return s, p
	}
	// The final checkpoint is collected a second after the window closed, with nothing new.
	s, p := stage(redialWindow - 100*time.Millisecond)
	final := nativeBoundary(int(time.Since(s.c.started).Milliseconds())+1000, map[string]uint64{"a": 100}, nil)
	final.final = true
	s.observe(sampledBoundary{boundary: final})
	if p.removed {
		t.Fatal("a late final checkpoint removed a server that moved within the window")
	}
	for _, c := range []struct {
		sinceMovement time.Duration
		removed       bool
	}{{0, false}, {time.Second, true}} {
		s, p := stage(c.sinceMovement)
		_ = s.handle(resourceOutcome{server: s.servers[0], role: string(Down), err: errors.New("stream reset"),
			at: time.Now()})
		if p.removed != c.removed {
			t.Errorf("a failure %v after movement removed = %v, want %v", c.sinceMovement, p.removed, c.removed)
		}
	}
}

func TestAggregationMatchesTheSharedVectors(t *testing.T) {
	t.Parallel()
	data := apipin.Read(t, "aggregation.testvectors.json")
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
			ObservedUp map[string]struct {
				ID      string
				Maximum uint64
			}
		}
		Intervals []struct {
			Reason   IntervalReason
			Complete bool
			Window   *window
		}
		Result     results
		TotalBytes struct{ Down, Up uint64 }
		Servers    map[string]results
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
				a.dropout(live, time.Duration(b.AtMs)*time.Millisecond)
			}
			boundary := nativeBoundary(int(b.AtMs), b.Down, map[string]*ReceiverSnapshot{})
			boundary.final, boundary.observedUp = b.Final, map[string]uploadLedger{}
			for id, o := range b.ObservedUp {
				boundary.observedUp[id] = uploadLedger{o.ID, o.Maximum}
			}
			for id, r := range b.Up {
				if r != nil {
					boundary.up[id] = &ReceiverSnapshot{ID: r.ID, Bytes: r.Bytes, Nanos: r.Nanos}
				}
			}
			a.observe(boundary)
		}
		reported := func(result Result) *reported {
			if !result.Unavailable {
				return &reported{result.MeanBps, result.PeakBps}
			}
			return nil
		}
		if got := (results{reported(a.result(Down)), reported(a.result(Up))}); !reflect.DeepEqual(got, c.Result) {
			t.Errorf("%s: result down=%+v up=%+v, want down=%+v up=%+v", c.Name, got.Down, got.Up,
				c.Result.Down, c.Result.Up)
		}
		totals := struct{ Down, Up uint64 }{a.result(Down).TotalBytes, a.result(Up).TotalBytes}
		if totals != c.TotalBytes {
			t.Errorf("%s: total bytes %+v, want %+v", c.Name, totals, c.TotalBytes)
		}
		for id, want := range c.Servers {
			got := results{reported(a.serverResult(id, Down)), reported(a.serverResult(id, Up))}
			if !reflect.DeepEqual(got, want) {
				t.Errorf("%s: server %s down=%+v up=%+v, want down=%+v up=%+v", c.Name, id, got.Down, got.Up,
					want.Down, want.Up)
			}
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
		s := testStage(p, StagePlan{Name: StageUpload, Directions: []Direction{Up}}, func(Event) {})
		s.ctx, s.c.started = t.Context(), begun
		initial, _ := s.collect(t.Context(), s.c.active(), checkpointBudget)
		co := s.c
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

// The stage end lands microseconds after the last tick, so its boundary spans a few reads.
func TestAStageEndBurstStaysOffTheLiveRate(t *testing.T) {
	t.Parallel()
	const burst = 6 * laneBuffer
	for _, dir := range []Direction{Down, Up} {
		t.Run(string(dir), func(t *testing.T) {
			t.Parallel()
			p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}},
				transport: &runner{coordinated: &participantCounters{}}}
			plan := StagePlan{Name: StageDownload, Directions: []Direction{Down}}
			if dir == Up {
				plan = StagePlan{Name: StageUpload, Directions: []Direction{Up}}
			}
			var live []ThroughputSample
			s := testStage(p, plan, func(e Event) { live = append(live, e.Throughput) })
			s.ctx = t.Context()
			boundary := func(at time.Duration, bytes uint64) measurementBoundary {
				b := measurementBoundary{at: at}
				if dir == Up {
					b.up = map[string]*ReceiverSnapshot{"a": {ID: "r", Bytes: bytes, Nanos: uint64(at)}}
				} else {
					b.down = map[string]uint64{"a": bytes}
				}
				return b
			}
			s.c.aggregate.observe(boundary(time.Second, 0))
			s.beginSampling(time.Now())
			var steady []ThroughputSample
			for i := range uint64(4) {
				s.observe(sampledBoundary{boundary: boundary(time.Second+time.Duration(i+1)*sampleInterval,
					(i+1)*250_000_000)})
				steady = append(steady, ThroughputSample{BytesPerSec: 1e9, TotalBytes: (i + 1) * 250_000_000})
			}
			const total = 1_000_000_000 + burst
			final := boundary(2*time.Second+50*time.Microsecond, total)
			final.final = true
			s.observe(sampledBoundary{boundary: final})
			if !reflect.DeepEqual(live, steady) {
				t.Fatalf("live rates %+v, want %+v", live, steady)
			}
			result := s.c.aggregate.result(dir)
			if mean := total / (time.Second + 50*time.Microsecond).Seconds(); result.Unavailable ||
				result.MeanBps != mean || result.TotalBytes != total {
				t.Fatalf("the stage end left the result: %+v, want mean %v", result, mean)
			}
		})
	}
}

func TestLiveRatesRestartOnlyWithTheInterval(t *testing.T) {
	t.Parallel()
	p := &participant{prepared: PreparedServer{Server: wire.ServerEntry{ID: "a"}}}
	var live []ThroughputSample
	s := testStage(p, StagePlan{Name: StageUpload, Directions: []Direction{Up}},
		func(e Event) { live = append(live, e.Throughput) })
	s.c.aggregate.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("r1", 0, 1000)}))
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
