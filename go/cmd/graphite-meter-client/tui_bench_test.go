package main

import (
	"fmt"
	"math"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

var (
	tuiFrame string
	tuiLines []string
)

func benchmarkModel(b *testing.B, w, h int) model {
	b.Helper()
	cfg := goclient.DefaultConfig()
	cfg.Stages.Bidirectional = true
	m := newModel(cfg)
	b.Cleanup(m.controller.Close)
	m.width, m.height = w, h
	m.now = time.Now()
	m.run = newRunState(cfg, m.now.Add(-60*time.Second))
	r := m.run
	r.stage, r.phase, r.focus = goclient.StageBidirectional, goclient.PhaseMeasuring, "a"
	r.marks = []mark{{0, goclient.StageLatency}, {15, goclient.StageDownload}, {30, goclient.StageUpload}, {45, goclient.StageBidirectional}}
	r.details = &goclient.RunDetails{Participants: []string{"a", "b", "c", "d"}, LatencyFocus: "a"}
	for _, id := range r.details.Participants {
		r.details.Servers = append(r.details.Servers, goclient.ServerRunSummary{Server: wire.ServerEntry{ID: id, Name: "Server " + id}})
		for i := range 480 {
			r.rtt[id] = r.rtt[id].add(float64(i)/8, float64(time.Millisecond)*(8+math.Sin(float64(i)/10)))
		}
	}
	for _, dir := range []goclient.Direction{goclient.Down, goclient.Up} {
		r.rates[dir] = goclient.ThroughputSample{BytesPerSec: 1e8}
		r.shown[dir] = glide{to: 1e8}
		for i := range 480 {
			v := 1e8 * (1 + .1*math.Sin(float64(i)/12))
			if i%120 == 0 {
				v = math.NaN()
			}
			r.history[dir] = r.history[dir].add(float64(i)/8, v)
		}
	}
	for i := range r.stages {
		r.stages[i].state = stageDone
	}
	r.stages[3].state, r.stages[3].since = stageMeasuring, m.now.Add(-15*time.Second)
	return m
}

func BenchmarkTUIFrame(b *testing.B) {
	for _, size := range [][2]int{{80, 24}, {120, 40}, {160, 50}} {
		for _, mode := range []string{"animation", "sample"} {
			b.Run(fmt.Sprintf("%dx%d/%s", size[0], size[1], mode), func(b *testing.B) {
				m := benchmarkModel(b, size[0], size[1])
				tuiFrame = m.View().Content
				b.ReportAllocs()
				for b.Loop() {
					if mode == "sample" {
						m.run.history[goclient.Down] = m.run.history[goclient.Down].add(60, 1e8)
						m.run.rtt["a"] = m.run.rtt["a"].add(60, float64(8*time.Millisecond))
					}
					tuiFrame = m.View().Content
				}
			})
		}
	}
}

func BenchmarkTUIStrip(b *testing.B) {
	m := benchmarkModel(b, 120, 40)
	bands := [][]point{m.run.history[goclient.Down].points}
	b.ReportAllocs()
	for b.Loop() {
		tuiLines = m.st.strip(goclient.StageDownload, bands, m.run.stripTop(), 0, 60, 100, 4)
	}
}
