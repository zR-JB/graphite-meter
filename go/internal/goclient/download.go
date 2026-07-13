package goclient

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
)

func (r *runner) measureDownload(ctx context.Context, stage string, elapsed time.Duration, start <-chan struct{}) (Result, error) {
	path := r.routes().Download
	base, err := r.endpoint(path)
	if err != nil {
		return Result{}, err
	}
	laneCtx, laneCancel := context.WithCancel(ctx)
	defer laneCancel()
	var total atomic.Uint64
	var wg sync.WaitGroup
	stagger := r.laneStaggerStep()
	for i := 0; i < r.cfg.ParallelStreams; i++ {
		wg.Add(1)
		go func(lane int) {
			defer wg.Done()
			if !staggerSleep(laneCtx, lane, stagger) {
				return
			}
			r.downloadLane(laneCtx, base, lane, &total)
		}(i)
	}
	select {
	case <-ctx.Done():
		return Result{}, ctx.Err()
	case <-start:
	}
	measureCtx, cancel := context.WithTimeout(ctx, elapsed)
	stats := r.sampleLocalRates(measureCtx, stage, Down, &total, r.cfg.ParallelStreams)
	cancel()
	laneCancel()
	wg.Wait()
	return stats.result(stage, Down, false), nil
}

func (r *runner) downloadLane(ctx context.Context, base string, lane int, total *atomic.Uint64) {
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		u, err := url.Parse(base)
		if err != nil {
			return
		}
		q := u.Query()
		q.Set("bytes", strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10))
		q.Set("lane", strconv.Itoa(lane))
		q.Set("cb", strconv.FormatInt(time.Now().UnixNano(), 10))
		u.RawQuery = q.Encode()
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
		if err != nil {
			return
		}
		res, err := r.http.Do(req)
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			continue
		}
		if res.StatusCode != http.StatusOK {
			_ = res.Body.Close()
			return
		}
		for {
			n, readErr := res.Body.Read(buf)
			if n > 0 {
				total.Add(uint64(n))
			}
			if readErr != nil {
				_ = res.Body.Close()
				if errors.Is(readErr, io.EOF) {
					break
				}
				return
			}
			if ctx.Err() != nil {
				_ = res.Body.Close()
				return
			}
		}
	}
}

func (r *runner) sampleLocalRates(ctx context.Context, stage string, dir Direction, total *atomic.Uint64, streams int) rateStats {
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	lastN := total.Load()
	baseline := lastN
	lastT := time.Now()
	startT := lastT
	stats := rateStats{}
	for {
		select {
		case <-ctx.Done():
			stats.setWindow(total.Load()-baseline, time.Since(startT))
			return stats
		case now := <-ticker.C:
			n := total.Load()
			delta := n - lastN
			dt := now.Sub(lastT).Seconds()
			lastN = n
			lastT = now
			if delta == 0 || dt <= 0 {
				continue
			}
			bps := float64(delta) / dt
			measuredTotal := n - baseline
			stats.add(bps)
			r.emit(Event{
				Kind:      EventThroughput,
				At:        now,
				Stage:     stage,
				Direction: dir,
				Throughput: ThroughputSample{
					Stage:       stage,
					Direction:   dir,
					BytesPerSec: bps,
					TotalBytes:  measuredTotal,
					StreamCount: streams,
				},
			})
		}
	}
}

func unexpectedStatus(res *http.Response) error {
	if res == nil {
		return fmt.Errorf("empty HTTP response")
	}
	return fmt.Errorf("HTTP %d from %s", res.StatusCode, res.Request.URL.String())
}
