package goclient

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/server/internal/wire"
)

func Run(ctx context.Context, cfg Config, emit func(Event)) error {
	cfg = cfg.normalized()
	tr := &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     false,
		MaxIdleConns:          cfg.MaxIdleConnsPerHost * 2,
		MaxIdleConnsPerHost:   cfg.MaxIdleConnsPerHost,
		MaxConnsPerHost:       0,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: cfg.ResponseHeaderTimeout,
		ExpectContinueTimeout: cfg.ExpectContinueTimeout,
		TLSClientConfig:       &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
	}
	defer tr.CloseIdleConnections()
	hc := &http.Client{Transport: tr}

	pf, err := getPreflight(ctx, hc, cfg.BaseURL)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	emit(Event{Kind: EventPreflight, At: time.Now(), Preflight: &pf})

	r := runner{cfg: cfg, http: hc, preflight: pf, emit: emit}
	if cfg.Stages.Latency {
		if err := r.runLatencyStage(ctx, "latency", false, cfg.LatencyDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Download {
		if err := r.runTransferStage(ctx, "download", []Direction{Down}, cfg.DownloadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Upload {
		if err := r.runTransferStage(ctx, "upload", []Direction{Up}, cfg.UploadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Bidirectional {
		if err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, cfg.BidirectionalDuration); err != nil {
			return r.fail(err)
		}
	}
	emit(Event{Kind: EventComplete, At: time.Now(), Message: "complete"})
	return nil
}

type runner struct {
	cfg       Config
	http      *http.Client
	preflight wire.Preflight
	emit      func(Event)
}

func (r *runner) fail(err error) error {
	if err == nil || errors.Is(err, context.Canceled) {
		return err
	}
	r.emit(Event{Kind: EventError, At: time.Now(), Err: err})
	return err
}

func (r *runner) runLatencyStage(ctx context.Context, stage string, underLoad bool, duration time.Duration) error {
	start, err := r.warmupGate(ctx, stage)
	if err != nil {
		return err
	}
	stats, err := r.measureLatency(ctx, stage, underLoad, duration, start)
	if err != nil {
		return err
	}
	res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
	r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
	return nil
}

func (r *runner) runTransferStage(ctx context.Context, stage string, dirs []Direction, duration time.Duration) error {
	start, err := r.warmupGate(ctx, stage)
	if err != nil {
		return err
	}

	stageCtx, cancelStage := context.WithCancel(ctx)
	defer cancelStage()

	var wg sync.WaitGroup
	errs := make(chan error, len(dirs)+1)
	results := make(chan Result, len(dirs))
	for _, dir := range dirs {
		dir := dir
		wg.Add(1)
		go func() {
			defer wg.Done()
			var res Result
			var err error
			if dir == Down {
				res, err = r.measureDownload(stageCtx, stage, duration, start)
			} else {
				res, err = r.measureUpload(stageCtx, stage, duration, start)
			}
			if err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
				errs <- err
				return
			}
			results <- res
		}()
	}

	latDone := make(chan struct{})
	if r.cfg.LoadedLatency {
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer close(latDone)
			stats, err := r.measureLatency(stageCtx, stage, true, duration, start)
			if err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
				errs <- err
				return
			}
			if stats.Count > 0 {
				res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
				r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
			}
		}()
	} else {
		close(latDone)
	}

	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	select {
	case <-done:
	case err := <-errs:
		cancelStage()
		<-done
		return err
	case <-ctx.Done():
		cancelStage()
		<-done
		return ctx.Err()
	}
	close(results)
	for res := range results {
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Direction: res.Direction, Result: &res})
	}
	<-latDone
	return nil
}

func (r *runner) warmupGate(ctx context.Context, stage string) (<-chan struct{}, error) {
	start := make(chan struct{})
	if r.cfg.Warmup <= 0 {
		r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
		close(start)
		return start, nil
	}
	r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "warmup"})
	go func() {
		timer := time.NewTimer(r.cfg.Warmup)
		defer timer.Stop()
		select {
		case <-ctx.Done():
		case <-timer.C:
			r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
			close(start)
		}
	}()
	return start, nil
}

func (r *runner) endpoint(path string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("empty endpoint path")
	}
	return httpEndpoint(r.cfg.BaseURL, path)
}
