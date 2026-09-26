package endpoint

import (
	"context"
	"errors"
	"io"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// LaneEnd is how the reason a lane ended reaches its peer on each transport (api/wire.md#lane-endings).
type LaneEnd struct {
	WS     int
	WT     uint32
	Reason string
}

var (
	endPeer     = LaneEnd{1000, 0, ""}
	endIdle     = LaneEnd{4001, 1, "idle"}
	endLifetime = LaneEnd{4002, 2, "lifetime"}
	endRevoked  = LaneEnd{1008, 3, "authentication required"}
	endShutdown = LaneEnd{1001, 4, "shutdown"}
)

var errIdle = errors.New("lane idle")

// EndOf reads why ctx ended; server is the process's lifetime.
func EndOf(ctx, server context.Context) LaneEnd {
	cause := context.Cause(ctx)
	switch {
	case server.Err() != nil:
		return endShutdown
	case auth.SessionEnded(ctx):
		return endRevoked
	case errors.Is(cause, errIdle):
		return endIdle
	case errors.Is(cause, context.DeadlineExceeded):
		return endLifetime
	}
	return endPeer
}

// Activity counts what the peer sent, so a quiet lane can be ended.
type Activity struct{ n atomic.Uint64 }

// WatchIdle ends ctx with errIdle once the peer is quiet for one to one and a half bounds.
func WatchIdle(parent context.Context, bound time.Duration) (context.Context, *Activity) {
	ctx, cancel := context.WithCancelCause(parent)
	a := &Activity{}
	go a.watch(ctx, cancel, bound)
	return ctx, a
}

func (a *Activity) Bump() {
	if a != nil {
		a.n.Add(1)
	}
}

func (a *Activity) watch(ctx context.Context, cancel context.CancelCauseFunc, bound time.Duration) {
	tick := time.Tick(bound / 2)
	last := a.n.Load()
	quiet := 0
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick:
			now := a.n.Load()
			if now != last {
				last, quiet = now, 0
				continue
			}
			if quiet++; quiet == 2 {
				cancel(errIdle)
				return
			}
		}
	}
}

// idleDeadline re-arms a lane's socket deadline as it moves, so it ends idle for wire.WTIdleBound but never
// past limit; each move is also its session's activity.
type idleDeadline struct {
	set   func(time.Time) error
	limit time.Time
	live  *Activity
	armed time.Time
}

func (d *idleDeadline) moved(now time.Time) {
	d.live.Bump()
	if d.set == nil || now.Sub(d.armed) <= wire.WTIdleBound/8 {
		return
	}
	deadline := now.Add(wire.WTIdleBound)
	if !d.limit.IsZero() && d.limit.Before(deadline) {
		deadline = d.limit
	}
	_ = d.set(deadline)
	d.armed = now
}

// idleWriter moves in pieces a slow but draining peer takes well within the idle bound.
type idleWriter struct {
	w     io.Writer
	idle  idleDeadline
	moved bool
}

const idleWritePiece = 16 * 1024

func (w *idleWriter) Write(p []byte) (int, error) {
	total := 0
	for len(p) > 0 {
		n, err := w.w.Write(p[:min(len(p), idleWritePiece)])
		if n > 0 {
			total += n
			w.moved = true
			w.idle.moved(time.Now())
		}
		if err != nil {
			return total, err
		}
		p = p[n:]
	}
	return total, nil
}
