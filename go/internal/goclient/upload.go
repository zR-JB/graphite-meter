package goclient

import (
	"bufio"
	"cmp"
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

	"github.com/zR-JB/graphite-meter/go/internal/route"
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
	progressURL, err := r.endpoint(route.UploadProgress)
	if err != nil {
		return err
	}
	progressURL = withUploadID(progressURL, id)
	progress := newUploadProgress(ctx, id)
	defer func() { r.endUpload(progress, progressURL, errors.Is(context.Cause(ctx), errHandover)) }()

	lane := func(ctx context.Context, i int, ready func()) error {
		return r.uploadLane(ctx, id, i, block, ready)
	}
	if r.target.Transport == wire.TransportWebTransport {
		host, err := newWTStageSession(ctx, func(ctx context.Context) (*wtSession, error) {
			return wtDial(ctx, r.cred, r.target.Origin, route.WTUpload, url.Values{"id": {id}})
		}, func(ctx context.Context, sess *wtSession) error {
			str, err := acceptUploadProgressWT(ctx, sess)
			if err == nil {
				progress.attach(wtProgressFeed(progress.ctx, str), nil)
			}
			return err
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
	} else if err := r.followUploadFeed(ctx, progress, progressURL); err != nil {
		return err
	}
	if err := progress.awaitReady(ctx); err != nil {
		return err
	}
	r.coordinated.upload.Store(progress)
	return r.runLanes(ctx, gate, Up, progress, lane)
}

func (r *runner) mintUploadID(ctx context.Context) (string, error) {
	u, err := r.endpoint(route.UploadSession)
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
	base, err := r.endpoint(route.Upload)
	if err != nil {
		return err
	}
	return persist(ctx, func(ctx context.Context) (bool, error) {
		u, err := endpointWithQuery(base, url.Values{
			"id":   {id},
			"lane": {strconv.Itoa(lane)},
			"cb":   {strconv.FormatInt(time.Now().UnixNano(), 10)},
		})
		if err != nil {
			return false, refusal{err}
		}
		body := &cyclingBody{ctx: ctx, block: block, remaining: transferBytesPerStream}
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, u, body)
		if err != nil {
			return false, refusal{err}
		}
		req.Header.Set("Content-Type", "application/octet-stream")
		req.ContentLength = transferBytesPerStream
		res, err := cmp.Or(r.uploadHTTP, r.http).Do(req)
		if err != nil {
			return body.moved.Load(), err
		}
		_, _ = io.Copy(io.Discard, res.Body)
		_ = res.Body.Close()
		if res.StatusCode != http.StatusOK {
			return false, refusal{unexpectedStatus(res)}
		}
		return body.moved.Load(), nil
	})
}

type cyclingBody struct {
	ctx       context.Context
	block     []byte
	off       int
	remaining int64
	moved     atomic.Bool
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
	b.moved.Store(true)
	return len(p), nil
}

func withUploadID(base, id string) string {
	u, err := endpointWithQuery(base, url.Values{"id": {id}})
	if err != nil {
		return base
	}
	return u
}

// uploadProgress follows one session's receiver feeds; the receiver's pair only moves forward.
type uploadProgress struct {
	id     string
	ctx    context.Context
	cancel context.CancelCauseFunc
	work   sync.WaitGroup
	ready  chan error
	mu     sync.Mutex
	bytes  uint64
	nanos  uint64
	next   chan struct{}
}

func newUploadProgress(ctx context.Context, id string) *uploadProgress {
	ctx, cancel := context.WithCancelCause(ctx)
	return &uploadProgress{id: id, ctx: ctx, cancel: cancel, ready: make(chan error, 1), next: make(chan struct{})}
}

func (p *uploadProgress) counters() (bytes, nanos uint64) {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.bytes, p.nanos
}

func (p *uploadProgress) advanced() <-chan struct{} {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.next
}

func (p *uploadProgress) advance(bytes, nanos uint64) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	if bytes < p.bytes || nanos < p.nanos {
		return false
	}
	p.bytes, p.nanos = bytes, nanos
	close(p.next)
	p.next = make(chan struct{})
	return true
}

func (p *uploadProgress) awaitReady(ctx context.Context) error {
	select {
	case err := <-p.ready:
		return err
	case <-ctx.Done():
		return context.Cause(ctx)
	}
}

func (p *uploadProgress) signalReady(err error) {
	select {
	case p.ready <- err:
	default:
	}
}

func (p *uploadProgress) attach(feed io.ReadCloser, reopen func(context.Context) (io.ReadCloser, error)) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ctx.Err() != nil {
		_ = feed.Close()
		return
	}
	p.work.Go(func() {
		for {
			opened := time.Now()
			p.read(feed)
			if reopen == nil {
				return
			}
			deadline := time.Now().Add(redialWindow)
			if time.Since(opened) < retryBackoff && !pause(p.ctx, retryBackoff) {
				return
			}
			err := restore(p.ctx, deadline, "upload progress", func(ctx context.Context) (err error) {
				feed, err = reopen(ctx)
				return err
			})
			if err != nil {
				p.cancel(err)
				return
			}
		}
	})
}

func (p *uploadProgress) read(feed io.ReadCloser) {
	defer feed.Close() //nolint:errcheck // receiver counters and scanner errors own the outcome
	scanner := bufio.NewScanner(feed)
	for scanner.Scan() {
		event, err := wire.DecodeUploadProgress(scanner.Bytes())
		switch {
		case err != nil:
		case event.Type == "ready":
			p.signalReady(nil)
		case event.Type == "error":
			p.signalReady(fmt.Errorf("upload progress: %s", event.Message))
			return
		case event.Type == "progress" || event.Type == "complete":
			if p.advance(event.Bytes, event.Nanos) && event.Type == "complete" {
				return
			}
		}
	}
	if err := scanner.Err(); err != nil {
		p.signalReady(fmt.Errorf("upload progress read: %w", err))
	} else {
		p.signalReady(errors.New("upload progress closed before ready"))
	}
}

func (p *uploadProgress) close() {
	p.mu.Lock()
	p.cancel(nil)
	p.mu.Unlock()
	p.work.Wait()
}

type progressFeed struct {
	io.Reader
	stop func()
}

func (f progressFeed) Close() error {
	f.stop()
	return nil
}

func (r *runner) followUploadFeed(ctx context.Context, p *uploadProgress, target string) error {
	reopen := func(recovery context.Context) (io.ReadCloser, error) {
		return r.openUploadFeed(p.ctx, recovery, target)
	}
	feed, err := reopen(ctx)
	if err == nil {
		p.attach(feed, reopen)
	}
	return err
}

func (r *runner) openUploadFeed(lifetime, recovery context.Context, target string) (io.ReadCloser, error) {
	ctx, cancel := context.WithCancel(lifetime)
	defer context.AfterFunc(recovery, cancel)()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
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
		_ = res.Body.Close()
		cancel()
		return nil, unexpectedStatus(res)
	}
	return progressFeed{res.Body, func() { _ = res.Body.Close(); cancel() }}, nil
}

const (
	uploadSettleQuiet = 250 * time.Millisecond
	uploadSettleBound = 4 * time.Second
)

func (r *runner) settleUpload() {
	ctx, cancel := context.WithTimeout(r.teardown, uploadSettleBound)
	defer cancel()
	var last *ReceiverSnapshot
	for {
		snapshot, err := r.receiverCheckpointOnce(ctx)
		if err != nil || last != nil && snapshot.Bytes == last.Bytes || !pause(ctx, uploadSettleQuiet) {
			return
		}
		last = snapshot
	}
}

func (r *runner) endUpload(p *uploadProgress, target string, settle bool) {
	defer p.close()
	if settle {
		r.settleUpload()
	}
	ctx, cancel := context.WithTimeout(r.teardown, time.Second)
	defer cancel()
	if req, err := http.NewRequestWithContext(ctx, http.MethodDelete, target, nil); err == nil {
		if res, err := r.http.Do(req); err == nil {
			_ = res.Body.Close()
		}
	}
}
