package goclient

import (
	"bufio"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/route"
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

func (r *runner) measureUpload(ctx context.Context, gate *stageGate) error {
	id, err := r.mintUploadID(ctx)
	if err != nil {
		return err
	}
	block := make([]byte, 1<<20)
	rand.Read(block)
	progressURL, err := r.endpoint(r.target.Routes.UploadProgress)
	if err != nil {
		return err
	}
	progressURL = withUploadID(progressURL, id)

	var progress *uploadProgress
	var lane laneFunc
	if r.target.Transport == wire.TransportWebTransport {
		host, err := newWTStageSession(ctx, func(ctx context.Context) (*wtSession, error) {
			return wtDial(ctx, r.cfg, r.target.Origin, r.target.Routes.WTUpload, url.Values{"id": {id}})
		}, func(establishCtx context.Context, sess *wtSession) error {
			str, err := acceptUploadProgressWT(establishCtx, sess)
			if err != nil {
				return err
			}
			if progress == nil {
				progress, err = r.readUploadProgress(ctx, id, progressURL, wtProgressFeed(str))
				return err
			}
			progress.attach(wtProgressFeed(str))
			return nil
		})
		if err != nil {
			return err
		}
		defer host.close()
		lane = func(ctx context.Context, _ int, ready func()) error {
			return runWTLane(ctx, host, func(ctx context.Context, sess *wtSession) (bool, error) {
				return uploadLaneWT(ctx, sess, block, ready)
			})
		}
	} else {
		if progress, err = r.openUploadProgress(ctx, id, progressURL); err != nil {
			return err
		}
		lane = func(ctx context.Context, i int, ready func()) error {
			return r.uploadLane(ctx, id, i, block, ready)
		}
	}
	defer progress.bye(r.teardown)
	r.coordinated.upload.Store(progress)
	return r.runLanes(ctx, gate, Up, progress, lane)
}

func (r *runner) mintUploadID(ctx context.Context) (string, error) {
	u, err := r.endpoint(r.target.Routes.UploadSession)
	if err != nil {
		return "", err
	}
	var out uploadSessionResponse
	if _, err := controlJSON(ctx, r.http, http.MethodPost, u, "upload session", &out); err != nil {
		return "", err
	}
	if out.UploadID == "" || len(out.UploadID) > 8192 {
		return "", fmt.Errorf("upload session returned invalid uploadId")
	}
	return out.UploadID, nil
}

func (r *runner) uploadLane(ctx context.Context, id string, lane int, block []byte, ready func()) error {
	ctx = httptrace.WithClientTrace(ctx, &httptrace.ClientTrace{WroteHeaders: ready})
	base, err := r.endpoint(r.target.Routes.Upload)
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
		body := &cyclingBody{ctx: ctx, block: block, remaining: transferBytesPerStream}
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, u, body)
		if err != nil {
			return err
		}
		req.Header.Set("Content-Type", "application/octet-stream")
		req.ContentLength = transferBytesPerStream
		res, err := r.http.Do(req)
		if err != nil {
			pause(ctx, retryBackoff)
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

type cyclingBody struct {
	ctx       context.Context
	block     []byte
	off       int
	remaining int64
}

func (b *cyclingBody) Read(p []byte) (int, error) {
	if err := b.ctx.Err(); err != nil {
		return 0, err
	}
	if b.remaining <= 0 {
		return 0, io.EOF
	}
	p = p[:min(int64(len(p)), b.remaining)]
	for n := 0; n < len(p); {
		copied := copy(p[n:], b.block[b.off:])
		n += copied
		b.off = (b.off + copied) % len(b.block)
	}
	b.remaining -= int64(len(p))
	return len(p), nil
}

type uploadProgress struct {
	id, url string
	client  *http.Client
	ctx     context.Context // read lifetime; re-attachments stop with it
	cancel  context.CancelFunc
	mu      sync.Mutex
	work    sync.WaitGroup
	body    *uploadFeed
	done    chan struct{}
	ready   chan error
	count   atomic.Pointer[uploadCount]
	seq     atomic.Uint64
	changed chan struct{}
	errs    chan error
	once    sync.Once
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

func (r *runner) openUploadProgress(ctx context.Context, id, target string) (*uploadProgress, error) {
	body, err := r.openUploadFeed(ctx, ctx, target)
	if err != nil {
		return nil, err
	}
	p, err := r.readUploadProgress(ctx, id, target, body)
	if err != nil {
		return nil, err
	}
	p.work.Go(func() { r.reattachUploadProgress(p, target) })
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

// openUploadFeed bounds the request by recoveryCtx but lets the open feed live as long as stageCtx.
func (r *runner) openUploadFeed(stageCtx, recoveryCtx context.Context, target string) (*uploadFeed, error) {
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
		_ = res.Body.Close()
		cancelAttempt()
		return nil, unexpectedStatus(res)
	}
	return &uploadFeed{ReadCloser: res.Body, interrupt: cancelAttempt}, nil
}

func (r *runner) reattachUploadProgress(p *uploadProgress, target string) {
	for {
		opened := time.Now()
		_, ended := p.current()
		select {
		case <-p.ctx.Done():
			return
		case <-ended:
		}
		deadline := time.Now().Add(redialWindow)
		if time.Since(opened) < retryBackoff && !pause(p.ctx, retryBackoff) {
			return
		}
		err := restore(p.ctx, deadline, "upload progress", func(ctx context.Context) error {
			body, err := r.openUploadFeed(p.ctx, ctx, target)
			if err == nil {
				p.attach(body)
			}
			return err
		})
		if err != nil {
			if p.ctx.Err() == nil {
				p.fail(err)
			}
			return
		}
	}
}

func (r *runner) readUploadProgress(ctx context.Context, id, target string, body *uploadFeed) (*uploadProgress, error) {
	readCtx, cancel := context.WithCancel(ctx)
	p := &uploadProgress{
		id:      id,
		url:     target,
		client:  r.http,
		ctx:     readCtx,
		cancel:  cancel,
		ready:   make(chan error, 1),
		changed: make(chan struct{}, 1),
		errs:    make(chan error, 1),
	}
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
	p.body, p.done = body, done
	p.work.Go(func() { p.read(body, done) })
	p.mu.Unlock()
	if old != nil {
		old.interrupt()
	}
}

func (p *uploadProgress) signalReady(err error) {
	select {
	case p.ready <- err:
	default:
	}
}

func (p *uploadProgress) read(body *uploadFeed, done chan struct{}) {
	defer close(done)
	defer body.Close() //nolint:errcheck // receiver counters and scanner errors own the outcome
	scanner := bufio.NewScanner(body)
	for scanner.Scan() {
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
		p.signalReady(errors.New("upload progress closed before ready"))
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
			select {
			case err := <-p.errs:
				return err
			default:
				return errors.New("upload progress did not advance")
			}
		case <-p.changed:
		}
	}
	return nil
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
	if body, _ := p.current(); body != nil {
		body.interrupt()
	}
}

func (p *uploadProgress) close() {
	p.once.Do(func() {
		p.cancel()
		// interruptBody takes mu after cancellation, so attach cannot start another reader during Wait.
		p.interruptBody()
		p.work.Wait()
	})
}

func (p *uploadProgress) bye(teardown context.Context) {
	ctx, cancel := context.WithTimeout(teardown, time.Second)
	defer cancel()
	if req, err := http.NewRequestWithContext(ctx, http.MethodDelete, p.url, nil); err == nil {
		if res, err := p.client.Do(req); err == nil {
			_ = res.Body.Close()
		}
	}
	p.close()
}
