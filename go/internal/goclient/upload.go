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

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type uploadSessionResponse struct {
	UploadID string `json:"uploadId"`
}

func (r *runner) measureUpload(ctx context.Context, stage string, duration time.Duration, start <-chan struct{}) (Result, error) {
	id, err := r.mintUploadID(ctx)
	if err != nil {
		return Result{}, err
	}
	bodyBlock := make([]byte, 1024*1024)
	if _, err := rand.Read(bodyBlock); err != nil {
		return Result{}, err
	}

	progressURL, err := r.endpoint(r.routes().UploadProgress)
	if err != nil {
		return Result{}, err
	}
	progressURL = withUploadID(progressURL, id)

	var progress *uploadProgress
	var lane func(context.Context, int) error
	if r.targetTransport() == wire.TransportWebTransport {
		// The lanes and their counter share one session, so progress reports the
		// connection actually under test. A replacement session re-attaches the
		// same feed: the server keeps one aggregate per id, so the counters and
		// the measurement baseline carry across.
		host, err := newWTStageSession(ctx, func(dialCtx context.Context) (*wtSession, error) {
			return wtDial(dialCtx, r.cfg, r.target.Origin, r.routes().WTUpload, url.Values{"id": {id}})
		}, func(establishCtx context.Context, sess *wtSession) error {
			str, err := acceptUploadProgressWT(establishCtx, sess)
			if err != nil {
				return err
			}
			if progress == nil {
				progress, err = r.readUploadProgress(ctx, wtProgressStream{str}, progressURL)
				return err
			}
			progress.attach(wtProgressStream{str})
			return nil
		})
		if err != nil {
			return Result{}, err
		}
		defer host.close()
		// A feed can end while its session lives: the server reaps an idle
		// aggregate, or refuses after ready. HTTP re-attaches to the same
		// aggregate, so the counter survives either.
		go r.reattachUploadProgress(progress, progressURL)
		lane = func(laneCtx context.Context, _ int) error {
			return runWTLane(laneCtx, host, func(lctx context.Context, sess *wtSession) error {
				return r.uploadLaneWT(lctx, sess, bodyBlock)
			})
		}
	} else {
		if progress, err = r.openUploadProgress(ctx, progressURL); err != nil {
			return Result{}, err
		}
		lane = func(laneCtx context.Context, i int) error {
			return r.uploadLane(laneCtx, id, i, bodyBlock)
		}
	}
	defer progress.close()

	lanes := r.startLanes(ctx, lane)
	defer lanes.cancel()
	if err := lanes.waitStart(ctx, start); err != nil {
		return Result{}, err
	}
	if !progress.waitNext(ctx, progress.seq.Load()) {
		return Result{}, fmt.Errorf("upload progress did not advance")
	}
	baselineN := progress.n.Load()
	baselineT := progress.t.Load()
	stats, sampleErr := r.sampleServerUpload(ctx, stage, progress, r.streams, duration, baselineN, baselineT, lanes.errs)
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
		req, err := r.newKnownLengthUpload(ctx, u.String(), block)
		if err != nil {
			return err
		}
		res, err := r.http.Do(req)
		if err != nil {
			// A refused POST paces its retry, like every other reconnect path.
			if !laneRetryPause(ctx) {
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

// newKnownLengthUpload declares an exact Content-Length so the server reads
// large slices straight from the socket, matching the download's sized GET.
// Chunked framing drains the body in small pieces, roughly halving upload
// throughput on a fast link.
func (r *runner) newKnownLengthUpload(ctx context.Context, target string, block []byte) (*http.Request, error) {
	body := &cyclingBody{ctx: ctx, block: block, limit: r.cfg.UploadBytesPerStream}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, body)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/octet-stream")
	req.ContentLength = r.cfg.UploadBytesPerStream
	return req, nil
}

// cyclingBody is a request body that repeats block until it has emitted limit
// bytes, then returns io.EOF. A limit <= 0 cycles without end.
type cyclingBody struct {
	ctx   context.Context
	block []byte
	off   int
	limit int64 // total bytes to emit, <= 0 means unbounded
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

// http.NewRequest passes an io.ReadCloser body to the transport as-is instead
// of wrapping it in io.NopCloser.
var _ io.ReadCloser = (*cyclingBody)(nil)

type uploadProgress struct {
	ctx       context.Context // read lifetime; re-attachments stop with it
	cancel    context.CancelFunc
	client    *http.Client
	url       string
	mu        sync.Mutex // guards body and done across re-attachments
	body      io.ReadCloser
	done      chan struct{}
	ready     chan error
	readySent atomic.Bool
	n         atomic.Uint64
	t         atomic.Uint64
	seq       atomic.Uint64
	changed   chan struct{}
	once      sync.Once
}

type uploadProgressEvent struct {
	Type    string `json:"type"`
	Bytes   uint64 `json:"bytes"`
	Nanos   uint64 `json:"nanos"`
	Message string `json:"message"`
}

// withUploadID appends the id to a progress URL, the address the DELETE that
// finalizes an upload is sent to whatever transport carried the bytes.
func withUploadID(base, id string) string {
	u, err := url.Parse(base)
	if err != nil {
		return base
	}
	q := u.Query()
	q.Set("id", id)
	u.RawQuery = q.Encode()
	return u.String()
}

func (r *runner) openUploadProgress(ctx context.Context, target string) (*uploadProgress, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/x-ndjson")
	res, err := r.http.Do(req)
	if err != nil {
		return nil, err
	}
	if res.StatusCode != http.StatusOK {
		defer res.Body.Close()
		return nil, unexpectedStatus(res)
	}
	p, err := r.readUploadProgress(ctx, res.Body, target)
	if err != nil {
		return nil, err
	}
	go r.reattachUploadProgress(p, target)
	return p, nil
}

// reattachUploadProgress keeps the fetch feed alive across the server's
// request bound: when the GET dies with the stage still running, a fresh GET
// resumes the same aggregate, as the browser's progress worker does.
func (r *runner) reattachUploadProgress(p *uploadProgress, target string) {
	for {
		select {
		case <-p.ctx.Done():
			return
		case <-p.currentDone():
		}
		for p.ctx.Err() == nil {
			req, err := http.NewRequestWithContext(p.ctx, http.MethodGet, target, nil)
			if err != nil {
				return
			}
			req.Header.Set("Accept", "application/x-ndjson")
			res, err := r.http.Do(req)
			if err == nil && res.StatusCode == http.StatusOK {
				p.attach(res.Body)
				break
			}
			if res != nil {
				_ = res.Body.Close()
			}
			select {
			case <-p.ctx.Done():
				return
			case <-time.After(wtRedialBackoff):
			}
		}
	}
}

// readUploadProgress consumes the NDJSON feed from body, whichever transport
// carries it, and finalizes over HTTP at deleteURL.
func (r *runner) readUploadProgress(ctx context.Context, body io.ReadCloser, deleteURL string) (*uploadProgress, error) {
	readCtx, cancel := context.WithCancel(ctx)
	p := &uploadProgress{ctx: readCtx, cancel: cancel, client: r.http, url: deleteURL, ready: make(chan error, 1), changed: make(chan struct{}, 1)}
	context.AfterFunc(readCtx, p.closeBody)
	p.attach(body)
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

// attach replaces the feed's byte source with a stream from a replacement
// session. The server keeps one aggregate per id and the newest feed takes it
// over, so the counters and the measurement baseline carry across. A feed
// already closed takes no new reader: close() has run its one-shot cancel and
// would otherwise block forever on a reader installed behind it.
func (p *uploadProgress) attach(body io.ReadCloser) {
	done := make(chan struct{})
	p.mu.Lock()
	if p.ctx.Err() != nil {
		p.mu.Unlock()
		_ = body.Close()
		return
	}
	old := p.body
	p.body = body
	p.done = done
	p.mu.Unlock()
	if old != nil {
		_ = old.Close()
	}
	go p.read(body, done)
}

// signalReady delivers the first feed's ready-or-refused verdict exactly once;
// later attachments repeat the handshake records to an already-running stage.
func (p *uploadProgress) signalReady(err error) {
	if p.readySent.CompareAndSwap(false, true) {
		p.ready <- err
	}
}

func (p *uploadProgress) read(body io.ReadCloser, done chan struct{}) {
	defer close(done)
	scanner := bufio.NewScanner(body)
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
			p.signalReady(nil)
		case "progress", "complete":
			// A replaced feed's reader can still drain a record it had already
			// buffered; the server's count only moves forward.
			if event.Bytes < p.n.Load() {
				continue
			}
			p.n.Store(event.Bytes)
			p.t.Store(event.Nanos)
			p.seq.Add(1)
			select {
			case p.changed <- struct{}{}:
			default:
			}
		case "error":
			p.signalReady(fmt.Errorf("upload progress: %s", event.Message))
			return
		}
	}
	if err := scanner.Err(); err != nil {
		p.signalReady(fmt.Errorf("upload progress read: %w", err))
	} else {
		p.signalReady(fmt.Errorf("upload progress closed before ready"))
	}
}

// waitNext blocks until the server's counter advances past `after`. One feed
// ending is not the end of the report: a replacement session re-attaches to the
// same aggregate, so only this channel's own cancellation is terminal.
func (p *uploadProgress) waitNext(ctx context.Context, after uint64) bool {
	for p.seq.Load() <= after {
		select {
		case <-ctx.Done():
			return false
		case <-p.ctx.Done():
			return p.seq.Load() > after
		case <-p.changed:
		}
	}
	return true
}

func (p *uploadProgress) currentDone() chan struct{} {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.done
}

func (p *uploadProgress) closeBody() {
	p.mu.Lock()
	body := p.body
	p.mu.Unlock()
	if body != nil {
		_ = body.Close()
	}
}

func (p *uploadProgress) close() {
	p.once.Do(func() {
		p.cancel()
		p.closeBody()
		// nil when the feed was cancelled before its first reader was installed.
		if done := p.currentDone(); done != nil {
			<-done
		}
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
	case <-p.currentDone():
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
					Stage:       stage,
					Direction:   Up,
					BytesPerSec: bps,
					TotalBytes:  measuredTotal,
					StreamCount: streams,
					ServerAuth:  true,
				},
			})
		},
	}.run(ctx)
}
