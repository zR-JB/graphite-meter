package goclient

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (r *runner) measureDownload(ctx context.Context, gate *stageGate) error {
	total := &r.coordinated.down
	if r.target.Transport == wire.TransportWebTransport {
		host, err := newWTStageSession(ctx, func(ctx context.Context) (*wtSession, error) {
			return wtDial(ctx, r.cfg, r.target.Origin, r.target.Routes.WTDownload, r.wtDownloadQuery())
		}, nil)
		if err != nil {
			return err
		}
		defer host.close()
		return r.runLanes(ctx, gate, Down, nil, func(ctx context.Context, _ int, ready func()) error {
			buf := make([]byte, 1<<20)
			return runWTLane(ctx, host, func(ctx context.Context, sess *wtSession) (bool, error) {
				return downloadLaneWT(ctx, sess, buf, total, ready)
			})
		})
	}
	base, err := r.endpoint(r.target.Routes.Download)
	if err != nil {
		return err
	}
	return r.runLanes(ctx, gate, Down, nil, func(ctx context.Context, lane int, ready func()) error {
		return r.downloadLane(ctx, base, lane, total, ready)
	})
}

func (r *runner) downloadLane(ctx context.Context, base string, lane int, total *atomic.Uint64, ready func()) error {
	buf := make([]byte, 1<<20)
	for ctx.Err() == nil {
		u, err := endpointWithQuery(base, url.Values{
			"bytes": {strconv.FormatInt(transferBytesPerStream, 10)},
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
			pause(ctx, retryBackoff)
			continue
		}
		if res.StatusCode != http.StatusOK {
			_ = res.Body.Close()
			return unexpectedStatus(res)
		}
		ready()
		for ctx.Err() == nil {
			n, err := res.Body.Read(buf)
			total.Add(uint64(n))
			if err != nil {
				if !errors.Is(err, io.EOF) {
					pause(ctx, retryBackoff)
				}
				break
			}
		}
		_ = res.Body.Close()
	}
	return nil
}
