package goclient

import (
	"context"
	"fmt"
	"sync"
	"time"
)

// These fixture gates exercise individual lane recovery and receiver feed policies.
// Production runs have one nativeCoordinator stage schedule.
type transferOutcome struct {
	result  Result
	err     error
	latency bool
}

func (r *runner) runLatencyStage(ctx context.Context, stage string, underLoad bool, duration time.Duration) error {
	gate := r.newStageGate(ctx, stage, 1)
	defer gate.stop()
	stats, err := r.measureLatency(gate.ctx, stage, underLoad, duration, gate)
	if cause := context.Cause(gate.ctx); cause != nil {
		err = cause
	}
	if err == nil && !underLoad && stats.P50 > 0 {
		r.idleRTT = stats.P50
	}
	if err == nil || stats.HasObservations() {
		res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: stats.Elapsed, Err: err}
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: new(res)})
	}
	return err
}

func (r *runner) runTransferStage(ctx context.Context, stage string, dirs []Direction, duration time.Duration) error {
	participants := len(dirs)
	if r.cfg.LoadedLatency {
		participants++
	}
	gate := r.newStageGate(ctx, stage, participants)
	defer gate.stop()
	stageCtx := gate.ctx

	var wg sync.WaitGroup
	outcomes := make(chan transferOutcome, participants)
	for _, dir := range dirs {
		wg.Go(func() {
			res, err := r.measureDirection(stageCtx, stage, dir, duration, gate)
			outcomes <- transferOutcome{result: res, err: err}
		})
	}

	if r.cfg.LoadedLatency {
		wg.Go(func() {
			stats, err := r.measureLatency(stageCtx, stage, true, duration, gate)
			outcomes <- transferOutcome{result: Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: stats.Elapsed, Err: err}, err: err, latency: true}
		})
	}

	collected := make([]transferOutcome, 0, participants)
	for range participants {
		outcome := <-outcomes
		if outcome.err != nil {
			gate.cancel(outcome.err)
		}
		collected = append(collected, outcome)
	}
	wg.Wait()
	stageErr := context.Cause(stageCtx)
	for _, outcome := range collected {
		if !outcome.latency {
			continue
		}
		if stageErr != nil {
			outcome.result.Err = stageErr
		}
		if stageErr == nil || outcome.result.Latency.HasObservations() {
			r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: new(outcome.result)})
		}
	}
	for _, outcome := range collected {
		if outcome.latency || stageErr != nil && (outcome.result.TotalBytes == 0 || outcome.result.Elapsed <= 0) {
			continue
		}
		outcome.result.Err = stageErr
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Direction: outcome.result.Direction, Result: new(outcome.result)})
	}
	return stageErr
}

func (g *stageGate) stop() {
	g.cancel(nil)
	<-g.done
}

func (r *runner) newStageGate(ctx context.Context, stage string, participants int) *stageGate {
	ctx, cancel := context.WithCancelCause(ctx)
	g := &stageGate{ctx: ctx, cancel: cancel, ready: make(chan struct{}, participants), start: make(chan struct{}), done: make(chan struct{})}
	r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Phase: StagePreparing})
	go func() {
		defer close(g.done)
		prepareTimer := time.NewTimer(stageReadyTimeout)
		defer prepareTimer.Stop()
		for range participants {
			select {
			case <-ctx.Done():
				return
			case <-prepareTimer.C:
				cancel(fmt.Errorf("%s transports were not ready within %v", stage, stageReadyTimeout))
				return
			case <-g.ready:
			}
		}
		prepareTimer.Stop()
		if ctx.Err() != nil {
			return
		}
		if warmup := adaptiveWarmup(r.cfg.Warmup, r.idleRTT); warmup > 0 {
			r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Phase: StageWarmup})
			timer := time.NewTimer(warmup)
			defer timer.Stop()
			select {
			case <-ctx.Done():
				return
			case <-timer.C:
			}
		}
		if ctx.Err() != nil {
			return
		}
		r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Phase: StageMeasuring})
		close(g.start)
	}()
	return g
}
