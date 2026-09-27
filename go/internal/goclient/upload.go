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

func (r *runner) measureUpload(ctx context.Context, gate *stageGate) error {
	err := r.uploadReceiver(ctx, gate)
	// An unknown upload id grants one replacement receiver per server and run, as in the browser.
	if errors.Is(err, errUploadInvalid) && !r.replacedUpload {
		r.replacedUpload = true
		err = r.uploadReceiver(ctx, gate)
	}
	return err
}

func (r *runner) uploadReceiver(ctx context.Context, gate *stageGate) error {
	var id string
	err := restore(ctx, time.Now().Add(redialWindow), "upload session", func(ctx context.Context) (err error) {
		id, err = r.mintUploadID(ctx)
		return err
	})
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
			feed, err := wtProgressFeed(ctx, progress.ctx, sess)
			if err == nil {
				progress.attach(feed, nil)
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
	r.coordinated.upload.Store(progress)
	return r.runLanes(ctx, gate, Up, progress, lane)
}

func (r *runner) mintUploadID(ctx context.Context) (string, error) {
	u, err := r.endpoint(route.UploadSession)
	if err != nil {
		return "", err
	}
	var out wire.UploadSession
	if _, err := controlJSON(ctx, r.http, http.MethodPost, u, "upload session", &out); err != nil {
		return "", err
	}
	if err := out.Validate(); err != nil {
		return "", fmt.Errorf("%w: %w", errProtocol, err)
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
		_, _ = io.CopyN(io.Discard, res.Body, maxControlBytes)
		_ = res.Body.Close()
		idle := res.StatusCode == http.StatusRequestTimeout && res.Header.Get("X-Graphite-Upload-Refusal") == "idle"
		if res.StatusCode != http.StatusOK && !idle {
			return false, laneRefusal(res)
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

type uploadProgress struct {
	id     string
	ctx    context.Context
	cancel context.CancelCauseFunc
	work   sync.WaitGroup
	mu     sync.Mutex
	bytes  uint64
	nanos  uint64
	next   chan struct{}
}

func newUploadProgress(ctx context.Context, id string) *uploadProgress {
	ctx, cancel := context.WithCancelCause(ctx)
	return &uploadProgress{id: id, ctx: ctx, cancel: cancel, next: make(chan struct{})}
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

type progressFeed struct {
	records *bufio.Scanner
	from    string
	stop    func()
}

func openFeed(body io.Reader, from string, stop func()) (progressFeed, error) {
	feed := progressFeed{bufio.NewScanner(body), from, stop}
	for feed.records.Scan() {
		switch event, err := wire.DecodeUploadProgress(feed.records.Bytes()); {
		case err != nil:
		case event.Type == "ready":
			return feed, nil
		case event.Type == "error":
			stop()
			return progressFeed{}, uploadRefusal(event.Code, statusError{from: from})
		}
	}
	stop()
	return progressFeed{}, fmt.Errorf("%s closed before ready: %w", from, cmp.Or(feed.records.Err(), io.EOF))
}

func (p *uploadProgress) attach(feed progressFeed, reopen func(context.Context) (progressFeed, error)) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.ctx.Err() != nil {
		feed.stop()
		return
	}
	p.work.Go(func() {
		for {
			opened := time.Now()
			switch err := p.read(feed); {
			case permanent(err):
				p.cancel(err)
				return
			case err == nil || reopen == nil:
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

func (p *uploadProgress) read(feed progressFeed) error {
	defer feed.stop()
	for feed.records.Scan() {
		switch event, err := wire.DecodeUploadProgress(feed.records.Bytes()); {
		case err != nil:
		case event.Type == "error":
			return uploadRefusal(event.Code, statusError{from: feed.from})
		case event.Type == "progress" || event.Type == "complete":
			if p.advance(event.Bytes, event.Nanos) && event.Type == "complete" {
				return nil
			}
		}
	}
	return fmt.Errorf("%s ended: %w", feed.from, cmp.Or(feed.records.Err(), io.EOF))
}

func (p *uploadProgress) close() {
	p.mu.Lock()
	p.cancel(nil)
	p.mu.Unlock()
	p.work.Wait()
}

func (r *runner) followUploadFeed(ctx context.Context, p *uploadProgress, target string) error {
	reopen := func(recovery context.Context) (progressFeed, error) {
		return r.openUploadFeed(p.ctx, recovery, target)
	}
	var feed progressFeed
	err := restore(ctx, time.Now().Add(redialWindow), "upload progress", func(ctx context.Context) (err error) {
		feed, err = reopen(ctx)
		return err
	})
	if err == nil {
		p.attach(feed, reopen)
	}
	return err
}

func (r *runner) openUploadFeed(lifetime, recovery context.Context, target string) (progressFeed, error) {
	ctx, cancel := context.WithCancel(lifetime)
	defer context.AfterFunc(recovery, cancel)()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		cancel()
		return progressFeed{}, err
	}
	req.Header.Set("Accept", "application/x-ndjson")
	res, err := r.http.Do(req)
	if err != nil {
		cancel()
		return progressFeed{}, err
	}
	stop := func() { _ = res.Body.Close(); cancel() }
	if res.StatusCode != http.StatusOK {
		stop()
		return progressFeed{}, unexpectedStatus(res)
	}
	return openFeed(res.Body, "upload progress", stop)
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
