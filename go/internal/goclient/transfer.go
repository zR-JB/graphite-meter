package goclient

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"sync"
	"sync/atomic"
	"time"
)

const (
	retryBackoff = 500 * time.Millisecond
	redialWindow = 2 * time.Second
	laneStagger  = 75 * time.Millisecond
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
	windowCtx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	var lastErr error
	for {
		err := attempt(windowCtx)
		if err == nil {
			return nil
		}
		if _, authRequired := errors.AsType[*AuthRequiredError](err); authRequired {
			return err
		}
		if !errors.Is(err, context.DeadlineExceeded) || lastErr == nil {
			lastErr = err
		}
		if !pause(windowCtx, retryBackoff) {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return fmt.Errorf("%s lost and not replaced within %v: %w", what, redialWindow, lastErr)
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

func (g *laneGroup) wait(ctx context.Context, until <-chan struct{}, progressErr <-chan error) error {
	select {
	case <-ctx.Done():
		return context.Cause(ctx)
	case err := <-g.errs:
		return err
	case err := <-progressErr:
		return err
	case <-until:
		return nil
	}
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
) (failure error) {
	lanes := r.startLanes(ctx, r.streams.of(dir), lane)
	defer lanes.stop()
	defer func() {
		if failure != nil {
			gate.cancel(failure)
		}
	}()
	for range cap(lanes.ready) {
		if err := lanes.wait(ctx, lanes.ready, nil); err != nil {
			return err
		}
	}
	var progressErr <-chan error
	if progress != nil {
		progressErr = progress.errs
		if err := progress.waitNext(ctx, progress.seq.Load(), lanes.errs); err != nil {
			return err
		}
	}
	gate.reportReady()
	if err := lanes.wait(ctx, gate.start, progressErr); err != nil {
		return err
	}
	return lanes.wait(ctx, nil, progressErr)
}

func (r *runner) receiverCheckpoint(ctx context.Context) (*ReceiverSnapshot, error) {
	for {
		snapshot, err := r.receiverCheckpointOnce(ctx)
		if _, authRequired := errors.AsType[*AuthRequiredError](err); err == nil || authRequired {
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
		return nil, fmt.Errorf("upload receiver is not ready")
	}
	endpoint, err := r.endpoint(r.target.Routes.UploadCheckpoint)
	if err != nil {
		return nil, err
	}
	var count struct {
		Bytes uint64 `json:"bytes"`
		Nanos uint64 `json:"nanos"`
	}
	target := withUploadID(endpoint, id)
	if _, err := controlJSON(ctx, r.http, http.MethodPost, target, "receiver checkpoint", &count); err != nil {
		return nil, err
	}
	if count.Nanos == 0 || count.Nanos > uint64(1<<63-1) {
		return nil, fmt.Errorf("invalid receiver clock")
	}
	return &ReceiverSnapshot{ID: id, Bytes: count.Bytes, Nanos: count.Nanos}, nil
}
