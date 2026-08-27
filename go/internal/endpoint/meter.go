package endpoint

import (
	"context"
	"log"
	"sync/atomic"
	"time"
)

// Meter logs one transfer endpoint's aggregate throughput once per second under verbose mode.
type Meter struct {
	name  string
	bytes atomic.Int64 // cumulative bytes moved, ever
	conns atomic.Int64 // currently-open requests
}

// NewMeter builds a meter tagged with name (e.g. "server:download").
func NewMeter(name string) *Meter { return &Meter{name: name} }

func (m *Meter) Add(n int) {
	if m != nil {
		m.bytes.Add(int64(n))
	}
}

func (m *Meter) Open() {
	if m != nil {
		m.conns.Add(1)
	}
}

func (m *Meter) Close() {
	if m != nil {
		m.conns.Add(-1)
	}
}

// Run logs the per-second byte rate and live connection count until ctx is cancelled.
func (m *Meter) Run(ctx context.Context) {
	if m == nil {
		return
	}
	ticker := time.Tick(time.Second)
	var last int64
	lastTick := time.Now()
	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker:
			total := m.bytes.Load()
			delta := total - last
			last = total
			conns := m.conns.Load()
			if delta == 0 && conns == 0 {
				lastTick = now
				continue
			}
			window := now.Sub(lastTick).Seconds()
			lastTick = now
			gbit := float64(delta) * 8 / window / 1e9 // SI base-10, matching the client
			log.Printf("[gm:%s] %.2f Gbit/s · %d conns · %.2f MB this window",
				m.name, gbit, conns, float64(delta)/1e6)
		}
	}
}
