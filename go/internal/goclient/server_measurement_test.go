package goclient

import (
	"errors"
	"math"
	"testing"
	"time"
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
	a.begin("upload", []string{"a", "b"}, 0, "stage-start")
	a.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 100, 100),
		"b": nativeReceiver("b", 200, 100),
	}))
	sample := a.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 1100, 1100),
		"b": nativeReceiver("b", 6200, 2100),
	}))
	if sample == nil || *sample.UpBytesPerSec != 4000 {
		t.Fatalf("sum of receiver-window means = %+v, want 4000 B/s", sample)
	}
	result := a.result("upload", Up)
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
	a.begin("download", []string{"a", "b"}, 0, "stage-start")
	for i, bytes := range []uint64{0, 0, 1000, 1000, 2000, 2000, 3000} {
		a.observe(nativeBoundary(i*250, map[string]uint64{"a": bytes, "b": bytes / 2}, nil))
	}
	result := a.result("download", Down)
	if result.PeakBps != 3000 || result.MeanBps != 3000 || result.Samples != 6 {
		t.Fatalf("a burst inside a short window became the peak: %+v", result)
	}
	if a.serverPeaks[componentKey{"a", Down}] != 2000 || a.serverPeaks[componentKey{"b", Down}] != 1000 ||
		a.serverSamples["a"] != 6 {
		t.Fatalf("per-server peaks or samples = %v %v", a.serverPeaks, a.serverSamples)
	}
}
func TestCoordinatedOppositeFluctuationsAndLedger(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.begin("download", []string{"a", "b"}, 0, "stage-start")
	a.observe(nativeBoundary(0, map[string]uint64{"a": 0, "b": 0}, nil))
	a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000, "b": 3000}, nil))
	a.observe(nativeBoundary(2000, map[string]uint64{"a": 4000, "b": 4000}, nil))
	result := a.result("download", Down)
	if result.MeanBps != 4000 || result.PeakBps != 4000 || result.TotalBytes != 8000 {
		t.Fatalf("independent peaks or durations leaked into aggregate: %+v", result)
	}
	a.begin("download", []string{"b"}, 2*time.Second, "dropout")
	a.observe(nativeBoundary(2100, map[string]uint64{"b": 4200}, nil))
	a.observe(nativeBoundary(2500, map[string]uint64{"b": 5000}, nil))
	if result := a.result("download", Down); !result.Unavailable || result.TotalBytes != 9000 {
		t.Fatalf("late dropout must revoke headline without losing bytes: %+v", result)
	}
	if a.intervals[0].Window == nil || *a.intervals[0].Window.DownBytesPerSec != 4000 {
		t.Fatal("earlier evidence lost")
	}
}
func TestCoordinatedZeroMissingAndRecovery(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.begin("upload", []string{"a"}, 0, "stage-start")
	a.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 100, 100)}))
	a.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 100, 1100)}))
	if result := a.result("upload", Up); result.Unavailable || result.MeanBps != 0 {
		t.Fatalf("measured zero = %+v", result)
	}
	missing := nativeBoundary(1200, nil, map[string]*ReceiverSnapshot{"a": nil})
	missing.observedUp = map[string]uploadLedger{"a": {"id", 500}}
	a.observe(missing)
	a.observe(nativeBoundary(1300, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 500, 1100)}))
	if result := a.result("upload", Up); result.Unavailable || result.MeanBps != 0 || result.TotalBytes != 400 ||
		len(a.intervals) != 1 {
		t.Fatalf("a missing or stale checkpoint must be skipped, keeping bytes and the window: %+v", result)
	}
	a.observe(nativeBoundary(1500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 700, 1600)}))
	a.observe(nativeBoundary(2500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1700, 2600)}))
	if result := a.result("upload", Up); result.Unavailable || result.MeanBps != 640 || result.TotalBytes != 1600 ||
		len(a.intervals) != 1 {
		t.Fatalf("the next valid boundary must span the gap in the receiver clock: %+v", result)
	}
	a.observe(nativeBoundary(3500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("new", 100, 100)}))
	if len(a.intervals) != 2 || a.intervals[1].Reason != "evidence-resumed" ||
		a.result("upload", Up).TotalBytes != 1700 {
		t.Fatalf("a replaced receiver must start a fresh interval: %+v", a.intervals)
	}
}
func TestCoordinatedBidirectionalUsesCommonMembership(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	a.begin("bidirectional", []string{"a", "b"}, 0, "stage-start")
	a.observe(nativeBoundary(0, map[string]uint64{"a": 0, "b": 0}, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 0, 100),
		"b": nativeReceiver("b", 0, 100),
	}))
	a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000, "b": 2000}, map[string]*ReceiverSnapshot{
		"a": nativeReceiver("a", 1000, 1100),
		"b": nativeReceiver("b", 6000, 2100),
	}))
	if a.result("bidirectional", Down).MeanBps != 3000 || a.result("bidirectional", Up).MeanBps != 4000 {
		t.Fatal("bidirectional clocks were mixed")
	}
	a.begin("bidirectional", nil, time.Second, "dropout")
	if !a.result("bidirectional", Down).Unavailable || !a.result("bidirectional", Up).Unavailable {
		t.Fatal("all failed must not retain the earlier headline")
	}
}
func TestCoordinatedIntervalsStayBounded(t *testing.T) {
	t.Parallel()
	a := aggregateMeasurements{}
	for i := range 140 {
		a.begin("download", []string{"a"}, time.Duration(i)*time.Second, "stage-start")
		a.observe(nativeBoundary(i*1000, map[string]uint64{"a": 0}, nil))
		a.observe(nativeBoundary((i+1)*1000, map[string]uint64{"a": 1000}, nil))
	}
	if len(a.intervals) != maximumIntervals || a.omitted != 12 || math.IsNaN(a.result("download", Down).MeanBps) {
		t.Fatalf("bounds=%d omitted=%d", len(a.intervals), a.omitted)
	}
}

func TestCoordinatedReceiverRegressionRevokesRateAndRetainsBytes(t *testing.T) {
	t.Parallel()
	var measurements aggregateMeasurements
	measurements.begin("upload", []string{"a"}, 0, "stage-start")
	measurements.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1000, 1000)}))
	measurements.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 3000, 3000)}))
	before := measurements.result("upload", Up)
	if before.Unavailable || before.MeanBps != 1000 || before.TotalBytes != 2000 {
		t.Fatalf("receiver clock was replaced by the client clock: %+v", before)
	}
	measurements.observe(nativeBoundary(1500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 3000, 1500)}))
	after := measurements.result("upload", Up)
	if !after.Unavailable || after.TotalBytes != before.TotalBytes || measurements.intervals[0].Window == nil {
		t.Fatalf("regressed receiver clock retained a rate or lost earlier bytes: %+v", after)
	}
}

func TestCheckpointMissesRemoveServers(t *testing.T) {
	t.Parallel()
	s := &sampler{misses: map[string]int{}}
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
