package goclient

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"sync"
	"sync/atomic"
	"time"
)

type uploadSessionResponse struct {
	UploadID string `json:"uploadId"`
}

func (r *runner) measureUpload(ctx context.Context, stage string, elapsed time.Duration, start <-chan struct{}) (Result, error) {
	id, err := r.mintUploadID(ctx)
	if err != nil {
		return Result{}, err
	}
	progress, err := r.openUploadProgress(ctx, id)
	if err != nil {
		return Result{}, err
	}
	defer progress.close()

	bodyBlock := make([]byte, 1024*1024)
	if _, err := rand.Read(bodyBlock); err != nil {
		return Result{}, err
	}

	lanes := r.startLanes(ctx, func(laneCtx context.Context, lane int) error {
		return r.uploadLane(laneCtx, id, lane, bodyBlock)
	})
	defer lanes.cancel()
	if err := lanes.waitStart(ctx, start); err != nil {
		return Result{}, err
	}
	if !progress.waitNext(ctx, progress.seq.Load()) {
		return Result{}, fmt.Errorf("upload progress did not advance")
	}
	baselineN := progress.n.Load()
	baselineT := progress.t.Load()
	stats, sampleErr := r.sampleServerUpload(ctx, stage, progress, r.streams, elapsed, baselineN, baselineT, lanes.errs)
	lanes.stop()
	progress.bye()
	return stats.result(stage, Up, true), sampleErr
}

func (r *runner) mintUploadID(ctx context.Context) (string, error) {
	path := r.routes().UploadSession
	u, err := r.endpoint(path)
	if err != nil {
		return "", err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, u, nil)
	if err != nil {
		return "", err
	}
	res, err := r.http.Do(req)
	if err != nil {
		return "", err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		return "", unexpectedStatus(res)
	}
	var out uploadSessionResponse
	if err := json.NewDecoder(res.Body).Decode(&out); err != nil {
		return "", err
	}
	if out.UploadID == "" {
		return "", fmt.Errorf("upload session returned empty uploadId")
	}
	return out.UploadID, nil
}

func (r *runner) uploadLane(ctx context.Context, id string, lane int, block []byte) error {
	path := r.routes().Upload
	base, err := r.endpoint(path)
	if err != nil {
		return err
	}
	for ctx.Err() == nil {
		u, err := url.Parse(base)
		if err != nil {
			return err
		}
		q := u.Query()
		q.Set("id", id)
		q.Set("lane", strconv.Itoa(lane))
		q.Set("cb", strconv.FormatInt(time.Now().UnixNano(), 10))
		u.RawQuery = q.Encode()
		// A fixed Content-Length body (mirroring the download's fixed-size GET),
		// NOT a chunked stream: a chunked request forces the server to drain the
		// body through its chunk-framing reader in small pieces, roughly halving
		// upload throughput on a fast link. A known length lets the server read
		// large slices straight from the socket, so up matches down.
		body := &cyclingBody{ctx: ctx, block: block, limit: r.cfg.UploadBytesPerStream}
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, u.String(), body)
		if err != nil {
			return err
		}
		req.Header.Set("Content-Type", "application/octet-stream")
		req.ContentLength = r.cfg.UploadBytesPerStream
		res, err := r.http.Do(req)
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			continue
		}
		_, _ = io.Copy(io.Discard, res.Body)
		_ = res.Body.Close()
		if res.StatusCode != http.StatusOK {
			return unexpectedStatus(res)
		}
	}
	return nil
}

// cyclingBody is a request body that repeats block until it has emitted limit
// bytes, then returns io.EOF — so the POST carries an exact Content-Length. A
// limit <= 0 cycles without end.
type cyclingBody struct {
	ctx   context.Context
	block []byte
	off   int
	limit int64 // total bytes to emit before EOF; <= 0 means unbounded
	sent  int64
}

func (b *cyclingBody) Read(p []byte) (int, error) {
	if err := b.ctx.Err(); err != nil {
		return 0, err
	}
	if b.limit > 0 {
		if b.sent >= b.limit {
			return 0, io.EOF
		}
		if rem := b.limit - b.sent; int64(len(p)) > rem {
			p = p[:rem]
		}
	}
	n := 0
	for n < len(p) {
		copied := copy(p[n:], b.block[b.off:])
		n += copied
		b.off += copied
		if b.off >= len(b.block) {
			b.off = 0
		}
	}
	b.sent += int64(n)
	return n, nil
}

func (b *cyclingBody) Close() error { return nil }

type uploadProgress struct {
	cancel  context.CancelFunc
	body    io.ReadCloser
	client  *http.Client
	url     string
	done    chan struct{}
	ready   chan error
	n       atomic.Uint64
	t       atomic.Uint64
	seq     atomic.Uint64
	changed chan struct{}
	once    sync.Once
}

type uploadProgressEvent struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes"`
	Nanos   uint64 `json:"nanos"`
	Message string `json:"message"`
}

func (r *runner) openUploadProgress(ctx context.Context, id string) (*uploadProgress, error) {
	base, err := r.endpoint(r.routes().UploadProgress)
	if err != nil {
		return nil, err
	}
	u, err := url.Parse(base)
	if err != nil {
		return nil, err
	}
	q := u.Query()
	q.Set("id", id)
	u.RawQuery = q.Encode()
	readCtx, cancel := context.WithCancel(ctx)
	req, err := http.NewRequestWithContext(readCtx, http.MethodGet, u.String(), nil)
	if err != nil {
		cancel()
		return nil, err
	}
	req.Header.Set("Accept", "application/x-ndjson")
	res, err := r.http.Do(req)
	if err != nil {
		cancel()
		return nil, err
	}
	if res.StatusCode != http.StatusOK {
		cancel()
		defer res.Body.Close()
		return nil, unexpectedStatus(res)
	}
	p := &uploadProgress{cancel: cancel, body: res.Body, client: r.http, url: u.String(), done: make(chan struct{}), ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	go func() {
		defer close(p.done)
		scanner := bufio.NewScanner(res.Body)
		ready := false
		for scanner.Scan() {
			if len(scanner.Bytes()) == 0 {
				continue
			}
			var event uploadProgressEvent
			if json.Unmarshal(scanner.Bytes(), &event) != nil {
				continue
			}
			switch event.Type {
			case "ready":
				if !ready {
					ready = true
					p.ready <- nil
				}
			case "progress", "complete":
				p.n.Store(event.Bytes)
				p.t.Store(event.Nanos)
				p.seq.Add(1)
				select {
				case p.changed <- struct{}{}:
				default:
				}
			case "error":
				if !ready {
					p.ready <- fmt.Errorf("upload progress: %s", event.Message)
				}
				return
			}
		}
		if !ready {
			if err := scanner.Err(); err != nil {
				p.ready <- fmt.Errorf("upload progress read: %w", err)
			} else {
				p.ready <- fmt.Errorf("upload progress closed before ready")
			}
		}
	}()
	select {
	case err := <-p.ready:
		if err != nil {
			p.close()
			return nil, err
		}
	case <-ctx.Done():
		p.close()
		return nil, ctx.Err()
	}
	return p, nil
}

func (p *uploadProgress) waitNext(ctx context.Context, after uint64) bool {
	for p.seq.Load() <= after {
		select {
		case <-ctx.Done():
			return false
		case <-p.done:
			return p.seq.Load() > after
		case <-p.changed:
		}
	}
	return true
}

func (p *uploadProgress) close() {
	p.once.Do(func() {
		p.cancel()
		_ = p.body.Close()
		<-p.done
	})
}

func (p *uploadProgress) bye() {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodDelete, p.url, nil)
	if err == nil {
		if res, doErr := p.client.Do(req); doErr == nil {
			_ = res.Body.Close()
		}
	}
	select {
	case <-p.done:
	case <-ctx.Done():
	}
	p.close()
}

// sampleServerUpload measures from the server's byte and active-time counters,
// so the rate excludes time the server spent with nothing to read.
func (r *runner) sampleServerUpload(ctx context.Context, stage string, p *uploadProgress, streams int, duration time.Duration, baselineN, baselineT uint64, laneErr <-chan error) (rateStats, error) {
	lastN, lastT := baselineN, baselineT
	return rateLoop{
		duration: duration,
		laneErr:  laneErr,
		window: func(stats *rateStats) {
			n, elapsed := p.n.Load(), p.t.Load()
			if n >= baselineN && elapsed >= baselineT {
				stats.setWindow(n-baselineN, time.Duration(elapsed-baselineT)) //nosec G115 -- guarded elapsed >= baselineT; diff fits int64
			}
		},
		sample: func(now time.Time, stats *rateStats) {
			n := p.n.Load()
			active := p.t.Load()
			if n <= lastN || active <= lastT {
				return
			}
			dn := n - lastN
			dt := active - lastT
			lastN = n
			lastT = active
			bps := float64(dn) / (float64(dt) / float64(time.Second))
			measuredTotal := n - baselineN
			stats.add(bps)
			r.emit(Event{
				Kind:      EventThroughput,
				At:        now,
				Stage:     stage,
				Direction: Up,
				Throughput: ThroughputSample{
					Stage:         stage,
					Direction:     Up,
					BytesPerSec:   bps,
					TotalBytes:    measuredTotal,
					StreamCount:   streams,
					ServerAuth:    true,
					MeasurementAt: time.Duration(active - baselineT), //nosec G115 -- active >= baselineT (monotonic); diff fits int64
				},
			})
		},
	}.run(ctx)
}

var _ io.ReadCloser = (*cyclingBody)(nil)
