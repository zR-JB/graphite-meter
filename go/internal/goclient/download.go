package goclient

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (r *runner) measureDownload(ctx context.Context, stage string, duration time.Duration, start <-chan struct{}) (Result, error) {
	var total atomic.Uint64
	var lane func(context.Context, int) error
	if r.targetTransport() == wire.TransportWebTransport {
		// One session hosts every lane; each lane opens its own stream on it.
		sess, err := wtDial(ctx, r.cfg, r.target.Origin, r.routes().WTDownload, r.wtDownloadQuery())
		if err != nil {
			return Result{}, err
		}
		defer sess.close()
		lane = func(laneCtx context.Context, _ int) error {
			return r.downloadLaneWT(laneCtx, sess, &total)
		}
	} else {
		base, err := r.endpoint(r.routes().Download)
		if err != nil {
			return Result{}, err
		}
		lane = func(laneCtx context.Context, i int) error {
			return r.downloadLane(laneCtx, base, i, &total)
		}
	}
	lanes := r.startLanes(ctx, lane)
	defer lanes.cancel()
	if err := lanes.waitStart(ctx, start); err != nil {
		return Result{}, err
	}
	measureCtx, cancel := context.WithTimeout(ctx, duration)
	stats, err := r.sampleLocalRates(measureCtx, stage, Down, &total, r.streams, lanes.errs)
	cancel()
	lanes.stop()
	return stats.result(stage, Down, false), err
}

func (r *runner) downloadLane(ctx context.Context, base string, lane int, total *atomic.Uint64) error {
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		u, err := url.Parse(base)
		if err != nil {
			return err
		}
		q := u.Query()
		q.Set("bytes", strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10))
		q.Set("lane", strconv.Itoa(lane))
		q.Set("cb", strconv.FormatInt(time.Now().UnixNano(), 10))
		u.RawQuery = q.Encode()
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
		if err != nil {
			return err
		}
		res, err := r.http.Do(req)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			continue
		}
		if res.StatusCode != http.StatusOK {
			err := unexpectedStatus(res)
			_ = res.Body.Close()
			return err
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
				return readErr
			}
			if ctx.Err() != nil {
				_ = res.Body.Close()
				return nil
			}
		}
	}
	return nil
}

// sampleLocalRates measures from the bytes the lanes have read locally; ctx
// carries the measurement window as its deadline.
func (r *runner) sampleLocalRates(ctx context.Context, stage string, dir Direction, total *atomic.Uint64, streams int, laneErr <-chan error) (rateStats, error) {
	baseline := total.Load()
	lastN := baseline
	startT := time.Now()
	lastT := startT
	return rateLoop{
		cancelEndsWindow: true,
		laneErr:          laneErr,
		window: func(stats *rateStats) {
			stats.setWindow(total.Load()-baseline, time.Since(startT))
		},
		sample: func(now time.Time, stats *rateStats) {
			n := total.Load()
			delta := n - lastN
			dt := now.Sub(lastT).Seconds()
			lastN = n
			lastT = now
			if delta == 0 || dt <= 0 {
				return
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
		},
	}.run(ctx)
}

func unexpectedStatus(res *http.Response) error {
	if res == nil {
		return fmt.Errorf("empty HTTP response")
	}
	if err := authResponseError(res); err != nil {
		return err
	}
	return fmt.Errorf("HTTP %d from %s", res.StatusCode, res.Request.URL.String())
}
