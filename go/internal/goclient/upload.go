package goclient

import (
	"bufio"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptrace"
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

func (r *runner) measureUpload(ctx context.Context, stage string, duration time.Duration, gate *stageGate) (result Result, failure error) {
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
	var lane func(context.Context, int, func()) error
	if r.targetTransport() == wire.TransportWebTransport {
		host, err := newWTStageSession(ctx, func(dialCtx context.Context) (*wtSession, error) {
			return wtDial(dialCtx, r.cfg, r.target.Origin, r.routes().WTUpload, url.Values{"id": {id}})
		}, func(establishCtx context.Context, sess *wtSession) error {
			str, err := acceptUploadProgressWT(establishCtx, sess)
			if err != nil {
				return err
			}
			if progress == nil {
				progress, err = r.readUploadProgress(ctx, wtProgressFeed(str), progressURL)
				return err
			}
			progress.attach(wtProgressFeed(str))
			return nil
		})
		if err != nil {
			return Result{}, err
		}
		defer host.close()
		go r.reattachUploadProgress(progress, progressURL)
		lane = func(laneCtx context.Context, _ int, ready func()) error {
			return runWTLane(laneCtx, host, func(lctx context.Context, sess *wtSession) (bool, error) {
				return r.uploadLaneWT(lctx, sess, bodyBlock, ready)
			})
		}
	} else {
		if progress, err = r.openUploadProgress(ctx, progressURL); err != nil {
			return Result{}, err
		}
		lane = func(laneCtx context.Context, i int, ready func()) error {
			return r.uploadLane(laneCtx, id, i, bodyBlock, ready)
		}
	}
	defer progress.bye()
	if r.coordinated != nil {
		r.coordinated.attachUpload(id, progress)
	}

	streams := r.streams.of(Up)
	lanes := r.startLanes(ctx, streams, lane)
	defer lanes.stop()
	defer func() {
		if failure != nil {
			gate.cancel(failure)
		}
	}()
	if err := lanes.waitReady(ctx); err != nil {
		return Result{}, err
	}
	if err := progress.waitNext(ctx, progress.seq.Load(), lanes.errs); err != nil {
		return Result{}, err
	}
	gate.markReady()
	if err := lanes.waitStart(ctx, gate.start, progress.errs); err != nil {
		return Result{}, err
	}
	if r.coordinated != nil {
		return waitCoordinatedTransfer(ctx, lanes.errs, progress.errs)
	}
	// A connected feed can remain silent after warmup. Bound acquisition of the
	// receiver's baseline separately; its wait is not a measured server window.
	baselineBudget := min(duration, stageReadyTimeout)
	baselineCtx, cancelBaseline := context.WithTimeoutCause(ctx, baselineBudget,
		fmt.Errorf("upload receiver baseline unavailable within %v", baselineBudget))
	err = progress.waitNext(baselineCtx, progress.seq.Load(), lanes.errs)
	cancelBaseline()
	if err != nil {
		return Result{}, err
	}
	baselineN, baselineT := progress.counters()
	stats, sampleErr := r.sampleServerUpload(ctx, stage, progress, streams, duration, baselineN, baselineT, lanes.errs)
	if sampleErr == nil {
		sampleErr = windowCarriedBytes(ctx, stage, Up, stats)
	}
	return stats.result(stage, Up, true), sampleErr
}

func (r *runner) mintUploadID(ctx context.Context) (string, error) {
	path := r.routes().UploadSession
	u, err := r.endpoint(path)
	if err != nil {
		return "", err
	}
	var out uploadSessionResponse
	_, err = (jsonHTTPClient{r.http}).requestJSON(ctx, http.MethodPost, u, nil, nil, &out, unexpectedStatus)
	if err != nil {
		return "", err
	}
	if out.UploadID == "" || len(out.UploadID) > 8192 {
		return "", fmt.Errorf("upload session returned invalid uploadId")
	}
	return out.UploadID, nil
}

func (r *runner) uploadLane(ctx context.Context, id string, lane int, block []byte, ready func()) error {
	ctx = httptrace.WithClientTrace(ctx, &httptrace.ClientTrace{WroteHeaders: ready})
	path := r.routes().Upload
	base, err := r.endpoint(path)
	if err != nil {
		return err
	}
	for ctx.Err() == nil {
		u, err := endpointWithQuery(base, url.Values{
			"id":   {id},
			"lane": {strconv.Itoa(lane)},
			"cb":   {strconv.FormatInt(time.Now().UnixNano(), 10)},
		})
		if err != nil {
			return err
		}
		req, err := r.newKnownLengthUpload(ctx, u, block)
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
		_, _ = io.Copy(io.Discard, res.Body)
		_ = res.Body.Close()
		if res.StatusCode != http.StatusOK {
			return unexpectedStatus(res)
		}
	}
	return nil
}

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

var _ io.ReadCloser = (*cyclingBody)(nil)

type uploadProgress struct {
	ctx       context.Context // read lifetime; re-attachments stop with it
	cancel    context.CancelFunc
	client    *http.Client
	url       string
	mu        sync.Mutex // guards body and done across re-attachments
	body      *uploadFeed
	done      chan struct{}
	ready     chan error
	readySent atomic.Bool
	count     atomic.Pointer[uploadCount]
	seq       atomic.Uint64
	changed   chan struct{}
	errs      chan error
	once      sync.Once
}

type uploadCount struct{ bytes, nanos uint64 }

func (p *uploadProgress) counters() (bytes, nanos uint64) {
	if held := p.count.Load(); held != nil {
		return held.bytes, held.nanos
	}
	return 0, 0
}

func (p *uploadProgress) advance(bytes, nanos uint64) bool {
	next := &uploadCount{bytes: bytes, nanos: nanos}
	for {
		held := p.count.Load()
		if held != nil && (bytes < held.bytes || nanos < held.nanos) {
			return false
		}
		if p.count.CompareAndSwap(held, next) {
			return true
		}
	}
}

func withUploadID(base, id string) string {
	u, err := endpointWithQuery(base, url.Values{"id": {id}})
	if err != nil {
		return base
	}
	return u
}

func (r *runner) openUploadProgress(ctx context.Context, target string) (*uploadProgress, error) {
	body, err := r.openUploadProgressWithin(ctx, ctx, target)
	if err != nil {
		return nil, err
	}
	p, err := r.readUploadProgress(ctx, body, target)
	if err != nil {
		return nil, err
	}
	go r.reattachUploadProgress(p, target)
	return p, nil
}

// Interrupting a feed is distinct from closing its body. Its reader owns Close,
// after Read has returned; HTTP request cancellation interrupts an in-flight read.
type uploadFeed struct {
	io.ReadCloser
	interrupt context.CancelFunc
}

func (f *uploadFeed) Close() error {
	f.interrupt()
	return f.ReadCloser.Close()
}

func (r *runner) openUploadProgressWithin(stageCtx, recoveryCtx context.Context, target string) (*uploadFeed, error) {
	attemptCtx, cancelAttempt := context.WithCancel(stageCtx)
	stopRecovery := context.AfterFunc(recoveryCtx, cancelAttempt)
	defer stopRecovery()
	req, err := http.NewRequestWithContext(attemptCtx, http.MethodGet, target, nil)
	if err != nil {
		cancelAttempt()
		return nil, err
	}
	req.Header.Set("Accept", "application/x-ndjson")
	res, err := r.http.Do(req)
	if err != nil {
		cancelAttempt()
		return nil, err
	}
	if res.StatusCode != http.StatusOK {
		err := unexpectedStatus(res)
		_ = res.Body.Close()
		cancelAttempt()
		return nil, err
	}
	return &uploadFeed{ReadCloser: res.Body, interrupt: cancelAttempt}, nil
}

func (r *runner) reattachUploadProgress(p *uploadProgress, target string) {
	opened := time.Now()
	for {
		_, ended := p.current()
		select {
		case <-p.ctx.Done():
			return
		case <-ended:
		}
		if _, current := p.current(); current != ended {
			opened = time.Now()
			continue
		}
		recoveryCtx, cancelRecovery := context.WithTimeout(p.ctx, wtSessionRedialWindow)
		if time.Since(opened) < wtRedialBackoff && !laneRetryPause(recoveryCtx) && p.ctx.Err() != nil {
			cancelRecovery()
			return
		}
		var lastErr error
		for recoveryCtx.Err() == nil {
			if _, current := p.current(); current != ended {
				cancelRecovery()
				break
			}
			body, err := r.openUploadProgressWithin(p.ctx, recoveryCtx, target)
			if err == nil {
				if _, current := p.current(); current != ended {
					_ = body.Close()
					cancelRecovery()
					break
				}
				opened = time.Now()
				p.attach(body)
				cancelRecovery()
				break
			}
			if _, authRequired := errors.AsType[*AuthRequiredError](err); authRequired {
				cancelRecovery()
				p.fail(err)
				return
			}
			if !errors.Is(err, context.DeadlineExceeded) || lastErr == nil {
				lastErr = err
			}
			select {
			case <-p.ctx.Done():
				cancelRecovery()
				return
			case <-recoveryCtx.Done():
			case <-time.After(wtRedialBackoff):
			}
		}
		cancelRecovery()
		if recoveryCtx.Err() != nil && p.ctx.Err() == nil {
			_, current := p.current()
			if current != ended {
				continue
			}
			if lastErr == nil {
				lastErr = recoveryCtx.Err()
			}
			p.fail(fmt.Errorf("upload progress lost and not reattached within %v: %w", wtSessionRedialWindow, lastErr))
			return
		}
	}
}

func (r *runner) readUploadProgress(ctx context.Context, body *uploadFeed, deleteURL string) (*uploadProgress, error) {
	readCtx, cancel := context.WithCancel(ctx)
	p := &uploadProgress{ctx: readCtx, cancel: cancel, client: r.http, url: deleteURL, ready: make(chan error, 1), changed: make(chan struct{}, 1), errs: make(chan error, 1)}
	context.AfterFunc(readCtx, p.interruptBody)
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

func (p *uploadProgress) attach(body *uploadFeed) {
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
		old.interrupt()
	}
	go p.read(body, done)
}

func (p *uploadProgress) signalReady(err error) {
	if p.readySent.CompareAndSwap(false, true) {
		p.ready <- err
	}
}

func (p *uploadProgress) read(body *uploadFeed, done chan struct{}) {
	defer close(done)
	defer body.Close() //nolint:errcheck // receiver counters and scanner errors own the outcome
	scanner := bufio.NewScanner(body)
	for scanner.Scan() {
		if len(scanner.Bytes()) == 0 {
			continue
		}
		event, err := wire.DecodeUploadProgress(scanner.Bytes())
		if err != nil {
			continue
		}
		switch event.Type {
		case "ready":
			p.signalReady(nil)
		case "progress", "complete":
			if !p.advance(event.Bytes, event.Nanos) {
				continue
			}
			p.seq.Add(1)
			select {
			case p.changed <- struct{}{}:
			default:
			}
			if event.Type == "complete" {
				return
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

func (p *uploadProgress) waitNext(ctx context.Context, after uint64, laneErr <-chan error) error {
	for p.seq.Load() <= after {
		select {
		case <-ctx.Done():
			return context.Cause(ctx)
		case err := <-laneErr:
			return err
		case <-p.ctx.Done():
			if p.seq.Load() > after {
				return nil
			}
			return p.failure(fmt.Errorf("upload progress did not advance"))
		case <-p.changed:
		}
	}
	return nil
}

func (p *uploadProgress) failure(fallback error) error {
	select {
	case err := <-p.errs:
		return err
	default:
		return fallback
	}
}

func (p *uploadProgress) fail(err error) {
	select {
	case p.errs <- err:
	default:
	}
	p.cancel()
}

func (p *uploadProgress) current() (*uploadFeed, chan struct{}) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.body, p.done
}

func (p *uploadProgress) interruptBody() {
	body, _ := p.current()
	if body != nil {
		body.interrupt()
	}
}

func (p *uploadProgress) close() {
	p.once.Do(func() {
		p.cancel()
		p.interruptBody()
		_, done := p.current()
		if done != nil {
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
	_, done := p.current()
	select {
	case <-done:
	case <-ctx.Done():
	}
	p.close()
}

func (r *runner) sampleServerUpload(ctx context.Context, stage string, p *uploadProgress, streams int, duration time.Duration, baselineN, baselineT uint64, laneErr <-chan error) (rateStats, error) {
	lastN, lastT := baselineN, baselineT
	return rateLoop{
		duration: duration,
		laneErr:  laneErr,
		stageErr: p.errs,
		window: func(stats *rateStats) {
			n, elapsed := p.counters()
			n, elapsed = max(n, lastN), max(elapsed, lastT)
			if n >= baselineN && elapsed >= baselineT {
				stats.setWindow(n-baselineN, time.Duration(elapsed-baselineT)) //nosec G115 -- elapsed is monotonic and bounded
			}
		},
		sample: func(now time.Time, stats *rateStats) {
			n, active := p.counters()
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
