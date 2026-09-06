package goclient

import (
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
	a := aggregateMeasurements{}
	a.begin("upload", []string{"a", "b"}, 0, "stage-start")
	a.observe(nativeBoundary(0, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("a", 100, 100), "b": nativeReceiver("b", 200, 100)}))
	sample := a.observe(nativeBoundary(1000, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("a", 1100, 1100), "b": nativeReceiver("b", 6200, 2100)}))
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
}
func TestCoordinatedOppositeFluctuationsAndLedger(t *testing.T) {
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
	if result := a.result("upload", Up); !result.Unavailable || result.TotalBytes != 400 {
		t.Fatalf("missing rate must retain known unique bytes: %+v", result)
	}
	a.observe(nativeBoundary(1500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 700, 1600)}))
	a.observe(nativeBoundary(2500, nil, map[string]*ReceiverSnapshot{"a": nativeReceiver("id", 1700, 2600)}))
	if result := a.result("upload", Up); result.Unavailable || result.MeanBps != 1000 || result.TotalBytes != 1600 {
		t.Fatalf("recovery reuses stale baseline or double counts bytes: %+v", result)
	}
}
func TestCoordinatedBidirectionalUsesCommonMembership(t *testing.T) {
	a := aggregateMeasurements{}
	a.begin("bidirectional", []string{"a", "b"}, 0, "stage-start")
	a.observe(nativeBoundary(0, map[string]uint64{"a": 0, "b": 0}, map[string]*ReceiverSnapshot{"a": nativeReceiver("a", 0, 100), "b": nativeReceiver("b", 0, 100)}))
	a.observe(nativeBoundary(1000, map[string]uint64{"a": 1000, "b": 2000}, map[string]*ReceiverSnapshot{"a": nativeReceiver("a", 1000, 1100), "b": nativeReceiver("b", 6000, 2100)}))
	if a.result("bidirectional", Down).MeanBps != 3000 || a.result("bidirectional", Up).MeanBps != 4000 {
		t.Fatal("bidirectional clocks were mixed")
	}
	a.begin("bidirectional", nil, time.Second, "dropout")
	if !a.result("bidirectional", Down).Unavailable || !a.result("bidirectional", Up).Unavailable {
		t.Fatal("all failed must not retain the earlier headline")
	}
}
func TestCoordinatedIntervalsStayBounded(t *testing.T) {
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
