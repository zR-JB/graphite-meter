package goclient

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/route"
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
		if permanent(err) {
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

type refusal struct{ error }

func (r refusal) Unwrap() error { return r.error }

func permanent(err error) bool {
	_, refused := errors.AsType[refusal](err)
	_, auth := errors.AsType[*AuthRequiredError](err)
	return refused || auth
}

var errNoBytes = errors.New("no bytes moved")

type laneEnd string

func (e laneEnd) Error() string { return "the server ended the lane: " + string(e) }

// laneEnding reads api/wire.md#lane-endings: a revoked grant asks for sign-in, anything else is redialled.
func laneEnding(err error) error {
	ends := []laneEnd{"idle", "lifetime", "revoked", "shutdown"}
	i := slices.Index([]websocket.StatusCode{4001, 4002, 1008, 1001}, websocket.CloseStatus(err))
	if closed, ok := errors.AsType[*webtransport.SessionError](err); ok && closed.Remote {
		i = slices.Index([]webtransport.SessionErrorCode{1, 2, 3, 4}, closed.ErrorCode)
	}
	switch {
	case i < 0:
		return err
	case ends[i] == "revoked":
		return &AuthRequiredError{}
	}
	return ends[i]
}

type FailureReason string

const (
	FailureConnectionLost FailureReason = "connection-lost"
	FailureTimeout        FailureReason = "timeout"
	FailureSignIn         FailureReason = "sign-in-required"
)

var errStalled = errors.New("stopped delivering bytes")

func failureReason(err error) FailureReason {
	end, ended := errors.AsType[laneEnd](err)
	_, auth := errors.AsType[*AuthRequiredError](err)
	switch {
	case auth:
		return FailureSignIn
	case ended && end != "shutdown", errors.Is(err, errStalled), errors.Is(err, context.DeadlineExceeded):
		return FailureTimeout
	}
	return FailureConnectionLost
}

// persist repeats a lane until ctx ends; one that moves nothing for redialWindow ends with its last error.
func persist(ctx context.Context, attempt func(context.Context) (progressed bool, err error)) error {
	var failingSince time.Time
	for {
		started := time.Now()
		progressed, err := attempt(ctx)
		if ctx.Err() != nil {
			return nil
		}
		if permanent(err) {
			return err
		}
		if progressed {
			failingSince = time.Time{}
		} else if failingSince.IsZero() {
			failingSince = started
		}
		if !progressed && time.Since(failingSince) >= redialWindow {
			return cmp.Or(err, errNoBytes)
		}
		if (err != nil || !progressed) && time.Since(started) < retryBackoff && !pause(ctx, retryBackoff) {
			return nil
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
) (failure error) {
	lanes := r.startLanes(ctx, r.streams.of(dir), lane)
	defer lanes.stop()
	defer func() {
		if failure != nil {
			gate.cancel(failure)
		}
	}()
	var progressFailed <-chan struct{}
	wait := func(until <-chan struct{}) error {
		select {
		case <-ctx.Done():
			return context.Cause(ctx)
		case err := <-lanes.errs:
			return err
		case <-progressFailed:
			return context.Cause(progress.ctx)
		case <-until:
			return nil
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
		return nil, errors.New("upload receiver is not ready")
	}
	endpoint, err := r.endpoint(route.UploadCheckpoint)
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
		return nil, errors.New("invalid receiver clock")
	}
	return &ReceiverSnapshot{ID: id, Bytes: count.Bytes, Nanos: count.Nanos}, nil
}
