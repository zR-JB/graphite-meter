package endpoint

import (
	"context"
	"log"
	"sync/atomic"
	"time"
)

// Meter is an optional, verbose-mode throughput logger for a transfer endpoint
// (the server-side counterpart to the client's debug logging). Endpoints Add()
// bytes as they stream and Open()/Close() around each request; a background
// goroutine started by Run() logs the aggregate per-second rate plus the live
// connection count.
//
// Every method is nil-safe, so the endpoints call them unconditionally — when
// verbose logging is off the *Meter is nil and each call is a cheap nil check.
//
// Line shape (matches the client's "[gm:...]" tagging so both sides read alike):
//
//	[gm:server:download] 9.41 Gbit/s · 4 conns · 1.18 GB this window
type Meter struct {
	name  string
	bytes atomic.Int64 // cumulative bytes moved, ever
	conns atomic.Int64 // currently-open requests
}

// NewMeter builds a meter tagged with name (e.g. "server:download").
func NewMeter(name string) *Meter { return &Meter{name: name} }

// Add records n bytes moved. nil-safe.
func (m *Meter) Add(n int) {
	if m != nil {
		m.bytes.Add(int64(n))
	}
}

// Open / Close bracket an in-flight request so the log shows live concurrency.
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

// Run logs the per-second byte rate until ctx is cancelled. Start it once per
// meter in its own goroutine. A quiet second (no bytes, no open connections) is
// skipped so the log only speaks while a test is actually running. nil-safe.
func (m *Meter) Run(ctx context.Context) {
	if m == nil {
		return
	}
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	var last int64
	lastT := time.Now()
	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker.C:
			total := m.bytes.Load()
			delta := total - last
			last = total
			conns := m.conns.Load()
			if delta == 0 && conns == 0 {
				lastT = now
				continue
			}
			dt := now.Sub(lastT).Seconds()
			lastT = now
			gbit := float64(delta) * 8 / dt / 1e9 // SI base-10, matching the client
			log.Printf("[gm:%s] %.2f Gbit/s · %d conns · %.2f MB this window",
				m.name, gbit, conns, float64(delta)/1e6)
		}
	}
}
