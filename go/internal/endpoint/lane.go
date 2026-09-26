package endpoint

import (
	"context"
	"errors"
	"io"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
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

// idleDeadline re-arms a lane's socket deadline, capped at limit, and counts each move as session activity.
type idleDeadline struct {
	set   func(time.Time) error
	bound time.Duration
	limit time.Time
	live  *Activity
	armed time.Time
	mu    sync.Mutex
	ended bool
}

func (d *idleDeadline) moved(now time.Time) {
	d.live.Bump()
	if d.set == nil || now.Sub(d.armed) <= d.bound/8 {
		return
	}
	deadline := now.Add(d.bound)
	if !d.limit.IsZero() && d.limit.Before(deadline) {
		deadline = d.limit
	}
	d.mu.Lock()
	if !d.ended {
		_ = d.set(deadline)
	}
	d.mu.Unlock()
	d.armed = now
}

// endWith cuts a blocked read or write when ctx ends; its release outwaits the cut, as the controller dies.
func (d *idleDeadline) endWith(ctx context.Context) (release func()) {
	stop := context.AfterFunc(ctx, func() {
		d.mu.Lock()
		defer d.mu.Unlock()
		if !d.ended {
			d.ended = true
			_ = d.set(time.Now())
		}
	})
	return func() {
		stop()
		d.mu.Lock()
		d.ended = true
		d.mu.Unlock()
	}
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
