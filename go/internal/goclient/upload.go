package goclient

import (
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

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
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

	laneCtx, laneCancel := context.WithCancel(ctx)
	defer laneCancel()
	var wg sync.WaitGroup
	stagger := r.laneStaggerStep()
	for i := 0; i < r.streams; i++ {
		wg.Add(1)
		go func(lane int) {
			defer wg.Done()
			if !staggerSleep(laneCtx, lane, stagger) {
				return
			}
			r.uploadLane(laneCtx, id, lane, bodyBlock)
		}(i)
	}

	select {
	case <-ctx.Done():
		return Result{}, ctx.Err()
	case <-start:
	}
	if !progress.waitNext(ctx, progress.seq.Load()) {
		return Result{}, fmt.Errorf("upload progress did not advance")
	}
	baselineN := progress.n.Load()
	baselineT := progress.t.Load()
	stats := r.sampleServerUpload(ctx, stage, progress, r.streams, elapsed, baselineN, baselineT)
	laneCancel()
	wg.Wait()
	progress.bye()
	return stats.result(stage, Up, true), nil
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

func (r *runner) uploadLane(ctx context.Context, id string, lane int, block []byte) {
	path := r.routes().Upload
	base, err := r.endpoint(path)
	if err != nil {
		return
	}
	for ctx.Err() == nil {
		u, err := url.Parse(base)
		if err != nil {
			return
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
			return
		}
		req.Header.Set("Content-Type", "application/octet-stream")
		req.ContentLength = r.cfg.UploadBytesPerStream
		res, err := r.http.Do(req)
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			continue
		}
		_, _ = io.Copy(io.Discard, res.Body)
		_ = res.Body.Close()
	}
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
	conn   *websocket.Conn
	cancel context.CancelFunc
	done   chan struct{}
	n      atomic.Uint64
	t      atomic.Uint64
	seq    atomic.Uint64
}

func (r *runner) openUploadProgress(ctx context.Context, id string) (*uploadProgress, error) {
	if r.progressChannel == nil || r.progressChannel.Routes.UploadProgress == nil {
		return nil, fmt.Errorf("no upload progress channel selected")
	}
	base, err := wsEndpoint(r.progressChannel.Origin, *r.progressChannel.Routes.UploadProgress)
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
	conn, _, err := websocket.Dial(ctx, u.String(), &websocket.DialOptions{HTTPClient: r.websocketHTTP, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		return nil, err
	}
	readCtx, cancel := context.WithCancel(ctx)
	p := &uploadProgress{conn: conn, cancel: cancel, done: make(chan struct{})}
	_ = conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "ws"})))
	go func() {
		defer close(p.done)
		for {
			_, msg, err := conn.Read(readCtx)
			if err != nil {
				return
			}
			f, err := wire.Decode(string(msg))
			if err != nil {
				continue
			}
			switch f.Op {
			case wire.OpBytesReceived, wire.OpUploadComplete:
				p.n.Store(f.N)
				p.t.Store(f.Nanos)
				p.seq.Add(1)
			}
		}
	}()
	return p, nil
}

func (p *uploadProgress) waitNext(ctx context.Context, after uint64) bool {
	ticker := time.NewTicker(5 * time.Millisecond)
	defer ticker.Stop()
	for p.seq.Load() <= after {
		select {
		case <-ctx.Done():
			return false
		case <-p.done:
			return false
		case <-ticker.C:
		}
	}
	return true
}

func (p *uploadProgress) close() {
	p.cancel()
	_ = p.conn.Close(websocket.StatusNormalClosure, "")
	<-p.done
}

func (p *uploadProgress) bye() {
	_ = p.conn.Write(context.Background(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpBYE})))
	timer := time.NewTimer(time.Second)
	defer timer.Stop()
	select {
	case <-p.done:
	case <-timer.C:
	}
}

func (r *runner) sampleServerUpload(ctx context.Context, stage string, p *uploadProgress, streams int, duration time.Duration, baselineN, baselineT uint64) rateStats {
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	timer := time.NewTimer(duration)
	defer timer.Stop()
	var lastN, lastT uint64
	stats := rateStats{}
	lastN = baselineN
	lastT = baselineT
	finish := func() rateStats {
		n, elapsed := p.n.Load(), p.t.Load()
		if n >= baselineN && elapsed >= baselineT {
			stats.setWindow(n-baselineN, time.Duration(elapsed-baselineT))
		}
		return stats
	}
	for {
		select {
		case <-ctx.Done():
			return finish()
		case <-timer.C:
			return finish()
		case now := <-ticker.C:
			n := p.n.Load()
			active := p.t.Load()
			if n <= lastN || active <= lastT {
				continue
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
					MeasurementAt: time.Duration(active - baselineT),
				},
			})
		}
	}
}

var _ io.ReadCloser = (*cyclingBody)(nil)
