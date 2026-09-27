package goclient

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	retryBackoff   = 500 * time.Millisecond
	busyBackoff    = 300 * time.Millisecond
	busyBackoffCap = 1200 * time.Millisecond
	redialWindow   = 2 * time.Second
	laneStagger    = 75 * time.Millisecond
)

func pause(ctx context.Context, d time.Duration) bool {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

func restore(ctx context.Context, deadline time.Time, what string, attempt func(context.Context) error) error {
	window := time.Until(deadline).Round(time.Millisecond)
	windowCtx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	restored, cause := false, error(nil)
	err := persist(windowCtx, func(ctx context.Context) (bool, error) {
		err := attempt(ctx)
		if restored = err == nil; restored {
			cancel()
		} else if windowCtx.Err() == nil || cause == nil {
			cause = err
		}
		return false, err
	})
	switch {
	case restored:
		return nil
	case ctx.Err() != nil:
		return ctx.Err()
	case permanent(err):
		return err
	}
	return fmt.Errorf("%s lost and not replaced within %v: %w", what, window, cmp.Or(cause, err))
}

type refusal struct{ error }

func (r refusal) Unwrap() error { return r.error }

func permanent(err error) bool {
	_, refused := errors.AsType[refusal](err)
	return refused || IsAuthRequired(err)
}

// persist repeats a lane until ctx ends; one stalled for redialWindow, or still failing at the end, returns its error.
func persist(ctx context.Context, attempt func(context.Context) (progressed bool, err error)) error {
	var failingSince time.Time
	var busyDelay time.Duration
	var failing error
	for {
		started := time.Now()
		progressed, err := attempt(ctx)
		if ctx.Err() != nil {
			if progressed || err == nil {
				return nil
			}
			return failing
		}
		if permanent(err) {
			return err
		}
		if progressed {
			failingSince, failing = time.Time{}, nil
		} else {
			failingSince, failing = cmp.Or(failingSince, started), cmp.Or(err, errStalled)
		}
		if !progressed && time.Since(failingSince) >= redialWindow {
			return failing
		}
		var delay time.Duration
		if status, ok := errors.AsType[statusError](err); ok && status.busy() {
			busyDelay = min(busyBackoffCap, max(busyBackoff, 2*busyDelay))
			delay = min(busyBackoffCap, max(busyDelay, status.retryAfter))
		} else {
			busyDelay = 0
			if (err != nil || !progressed) && time.Since(started) < retryBackoff {
				delay = retryBackoff
			}
		}
		if delay > 0 && !pause(ctx, delay) {
			return failing
		}
	}
}

type participantCounters struct {
	down   atomic.Uint64
	upload atomic.Pointer[uploadProgress]
}

func (p *participantCounters) uploaded() (id string, bytes uint64) {
	progress := p.upload.Load()
	if progress == nil {
		return "", 0
	}
	bytes, _ = progress.counters()
	return progress.id, bytes
}

type laneFunc func(ctx context.Context, lane int, ready func()) error

type laneGroup struct {
	cancel context.CancelFunc
	wg     sync.WaitGroup
	errs   chan error
	ready  chan struct{}
}

func (r *runner) startLanes(ctx context.Context, streams int, body laneFunc) *laneGroup {
	laneCtx, cancel := context.WithCancel(ctx)
	g := &laneGroup{cancel: cancel, errs: make(chan error, streams), ready: make(chan struct{}, streams)}
	step := r.laneStaggerStep(streams)
	for lane := range streams {
		g.wg.Go(func() {
			if !pause(laneCtx, time.Duration(lane)*step) {
				return
			}
			if err := body(laneCtx, lane, sync.OnceFunc(func() { g.ready <- struct{}{} })); err != nil {
				select {
				case g.errs <- err:
				default:
				}
			}
		})
	}
	return g
}

func (r *runner) laneStaggerStep(streams int) time.Duration {
	if streams <= 1 {
		return 0
	}
	return min(adaptiveWarmup(r.cfg.Warmup, r.idleRTT)/2/time.Duration(streams-1), laneStagger)
}

func (g *laneGroup) stop() {
	g.cancel()
	g.wg.Wait()
}

func (r *runner) runLanes(
	ctx context.Context,
	gate *stageGate,
	dir Direction,
	progress *uploadProgress,
	lane laneFunc,
) error {
	lanes := r.startLanes(ctx, r.streams.of(dir), lane)
	defer lanes.stop()
	var progressFailed <-chan struct{}
	wait := func(until <-chan struct{}) error {
		select {
		case <-ctx.Done():
		case err := <-lanes.errs:
			return err
		case <-progressFailed:
			if ctx.Err() == nil {
				return context.Cause(progress.ctx)
			}
		case <-until:
			return nil
		}
		lanes.stop()
		select {
		case err := <-lanes.errs:
			return err
		default:
			return context.Cause(ctx)
		}
	}
	for range cap(lanes.ready) {
		if err := wait(lanes.ready); err != nil {
			return err
		}
	}
	if progress != nil {
		progressFailed = progress.ctx.Done()
		if err := wait(progress.advanced()); err != nil {
			return err
		}
	}
	gate.reportReady()
	if err := wait(gate.start); err != nil {
		return err
	}
	return wait(nil)
}

func (r *runner) receiverCheckpoint(ctx context.Context) (*ReceiverSnapshot, error) {
	for {
		snapshot, err := r.receiverCheckpointOnce(ctx)
		if err == nil || IsAuthRequired(err) {
			return snapshot, err
		}
		if !pause(ctx, 100*time.Millisecond) {
			return nil, err
		}
	}
}

func (r *runner) receiverCheckpointOnce(ctx context.Context) (*ReceiverSnapshot, error) {
	id, _ := r.coordinated.uploaded()
	if id == "" {
		return nil, errors.New("upload receiver is not ready")
	}
	endpoint, err := r.endpoint(route.UploadCheckpoint)
	if err != nil {
		return nil, err
	}
	var count wire.UploadCheckpoint
	target := withUploadID(endpoint, id)
	if _, err := controlJSON(ctx, r.http, http.MethodPost, target, "receiver checkpoint", &count); err != nil {
		return nil, err
	}
	if count.Nanos == 0 {
		return nil, fmt.Errorf("%w: the receiver clock has not started", errProtocol)
	}
	return &ReceiverSnapshot{ID: id, Bytes: count.Bytes, Nanos: count.Nanos}, nil
}
