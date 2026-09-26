package goclient

import (
	"encoding/json/v2"
	"math"
	"os"
	"testing"
	"time"
)

func TestLatencyMatchesTheSharedVectors(t *testing.T) {
	t.Parallel()
	data, err := os.ReadFile("../../../api/latency.testvectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name     string
		Outcomes []struct {
			RTTMs          float64 `json:"rttMs"`
			Timeout, Break bool
		}
		Expect struct {
			Replies, Timeouts, JitterPairs int
			TimeoutRatio, P50Ms, P95Ms     *float64
			JitterMs                       *float64
		}
	}
	if err := json.Unmarshal(data, &cases, json.MatchCaseInsensitiveNames(true)); err != nil {
		t.Fatal(err)
	}
	ms := func(d time.Duration, present bool) *float64 {
		if !present {
			return nil
		}
		return new(float64(d) / float64(time.Millisecond))
	}
	for _, c := range cases {
		var stats latencyStats
		for _, o := range c.Outcomes {
			if o.Break {
				stats.breakContinuity()
				continue
			}
			stats.add(time.Duration(o.RTTMs*float64(time.Millisecond)), o.Timeout, 0)
		}
		got := stats.snapshot()
		ratio, resolved := got.TimeoutRatio()
		for name, pair := range map[string][2]*float64{
			"timeout ratio": {new(ratio), c.Expect.TimeoutRatio},
			"p50":           {ms(got.P50, got.Count > 0), c.Expect.P50Ms},
			"p95":           {ms(got.P95, got.Count > 0), c.Expect.P95Ms},
			"jitter":        {ms(got.Jitter, got.JitterPairs > 0), c.Expect.JitterMs},
		} {
			if name == "timeout ratio" && !resolved {
				pair[0] = nil
			}
			if (pair[0] == nil) != (pair[1] == nil) || pair[0] != nil && math.Abs(*pair[0]-*pair[1]) > 1e-9 {
				t.Errorf("%s: %s = %v, want %v", c.Name, name, deref(pair[0]), deref(pair[1]))
			}
		}
		want := c.Expect
		if got.Count != want.Replies || got.Timeouts != want.Timeouts || got.JitterPairs != want.JitterPairs {
			t.Errorf("%s: %d replies, %d timeouts, %d pairs; want %d, %d, %d", c.Name, got.Count, got.Timeouts,
				got.JitterPairs, want.Replies, want.Timeouts, want.JitterPairs)
		}
	}
	if _, ok := (LatencyStats{Unresolved: 3, SendFailures: 2}).TimeoutRatio(); ok {
		t.Fatal("unresolved probes and local failures became resolved probes")
	}
}

func deref(v *float64) any {
	if v == nil {
		return nil
	}
	return *v
}
