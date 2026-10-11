package main

import (
	"encoding/json"
	"math"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// The terminal estimates the wire rate the browser does.
func TestWireRateMatchesTheSharedVectors(t *testing.T) {
	var vectors struct {
		Cases []struct {
			Scheme, Transport, Protocol string
			IPVersion                   int
			Multiplier                  float64
		}
	}
	if err := json.Unmarshal(apipin.Read(t, "wirerate.testvectors.json"), &vectors); err != nil {
		t.Fatal(err)
	}
	for _, c := range vectors.Cases {
		target := wire.ThroughputTarget{Origin: c.Scheme + "://meter.example", Transport: c.Transport, Protocol: c.Protocol}
		if got := wireMultiplier(target, c.IPVersion); math.Abs(got-c.Multiplier) > 1e-6 {
			t.Errorf("%+v: %v", c, got)
		}
	}
}
