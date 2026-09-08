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

func laneRetryPause(ctx context.Context) bool {
	select {
	case <-ctx.Done():
		return false
	case <-time.After(wtRedialBackoff):
		return true
	}
}

func (r *runner) measureDownload(ctx context.Context, gate *stageGate) (failure error) {
	var total atomic.Uint64
	r.coordinated.attachDownload(&total)
	var lane func(context.Context, int, func()) error
	if r.targetTransport() == wire.TransportWebTransport {
		host, err := newWTStageSession(ctx, func(dialCtx context.Context) (*wtSession, error) {
			return wtDial(dialCtx, r.cfg, r.target.Origin, r.routes().WTDownload, r.wtDownloadQuery())
		}, nil)
		if err != nil {
			return err
		}
		defer host.close()
		lane = func(laneCtx context.Context, _ int, ready func()) error {
			return runWTLane(laneCtx, host, func(lctx context.Context, sess *wtSession) (bool, error) {
				return r.downloadLaneWT(lctx, sess, &total, ready)
			})
		}
	} else {
		base, err := r.endpoint(r.routes().Download)
		if err != nil {
			return err
		}
		lane = func(laneCtx context.Context, i int, ready func()) error {
			return r.downloadLane(laneCtx, base, i, &total, ready)
		}
	}
	streams := r.streams.of(Down)
	lanes := r.startLanes(ctx, streams, lane)
	defer lanes.stop()
	defer func() {
		if failure != nil {
			gate.cancel(failure)
		}
	}()
	if err := lanes.waitReady(ctx); err != nil {
		return err
	}
	gate.reportReady()
	if err := lanes.waitStart(ctx, gate.start, nil); err != nil {
		return err
	}
	return waitCoordinatedTransfer(ctx, lanes.errs, nil)
}

func (r *runner) downloadLane(ctx context.Context, base string, lane int, total *atomic.Uint64, ready func()) error {
	buf := make([]byte, 1024*1024)
	for ctx.Err() == nil {
		u, err := endpointWithQuery(base, url.Values{
			"bytes": {strconv.FormatInt(r.cfg.DownloadBytesPerStream, 10)},
			"lane":  {strconv.Itoa(lane)},
			"cb":    {strconv.FormatInt(time.Now().UnixNano(), 10)},
		})
		if err != nil {
			return err
		}
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
		if err != nil {
			return err
		}
		res, err := r.http.Do(req)
		if err != nil {
			if !laneRetryPause(ctx) {
				return nil
			}
			continue
		}
		if res.StatusCode != http.StatusOK {
			err := unexpectedStatus(res)
			_ = res.Body.Close()
			return err
		}
		ready()
		for {
			n, readErr := res.Body.Read(buf)
			if n > 0 {
				total.Add(uint64(n))
			}
			if readErr != nil {
				_ = res.Body.Close()
				if !errors.Is(readErr, io.EOF) && !laneRetryPause(ctx) {
					return nil
				}
				break
			}
			if ctx.Err() != nil {
				_ = res.Body.Close()
				return nil
			}
		}
	}
	return nil
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
