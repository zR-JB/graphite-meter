package main

import (
	"encoding/json"
	"math"
	"testing"

	"github.com/zR-JB/graphite-meter/go/internal/apipin"
)

// The terminal presents the rate the browser does, step for step.
func TestLiveRateMatchesTheSharedVectors(t *testing.T) {
	var vectors struct {
		Cases []struct {
			Name  string
			Steps [][4]any
		}
	}
	if err := json.Unmarshal(apipin.Read(t, "liverate.testvectors.json"), &vectors); err != nil {
		t.Fatal(err)
	}
	for _, c := range vectors.Cases {
		var e liveRate
		for i, step := range c.Steps {
			changed := e.observe(step[0].(float64), step[1].(float64))
			want := step[2].(float64)
			if changed != step[3].(bool) || math.Abs(e.presented-want) > 1e-9*math.Max(1, want) {
				t.Fatalf("%s step %d: presented %v changed %v, want %v %v", c.Name, i, e.presented, changed, want, step[3])
			}
		}
	}
}
