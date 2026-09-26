package goclient

import (
	"context"
	"encoding/json/jsontext"
	jsonv2 "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestUploadProgressKeepsAForwardPair(t *testing.T) {
	t.Parallel()
	const held = `{"type":"progress","bytes":100,"nanos":10}`
	for _, c := range []struct {
		name         string
		feeds        [][]string
		bytes, nanos uint64
	}{
		{"bytes regress", [][]string{{held, `{"type":"progress","bytes":90,"nanos":20}`}}, 100, 10},
		{"clock regresses", [][]string{{held, `{"type":"progress","bytes":110,"nanos":9}`}}, 100, 10},
		{"malformed pair", [][]string{{held, `{"type":"complete","bytes":"200","nanos":20}`}}, 100, 10},
		{"missing clock", [][]string{{held, `{"type":"complete","bytes":200}`}}, 100, 10},
		{"equal pair", [][]string{{held, held}}, 100, 10},
		{"complete ends the feed", [][]string{{held, `{"type":"complete","bytes":130,"nanos":40}`,
			`{"type":"progress","bytes":140,"nanos":50}`}}, 130, 40},
		{"stale prefix of a replacement feed", [][]string{{held}, {`{"type":"progress","bytes":90,"nanos":5}`}},
			100, 10},
		{"replacement feed moves on", [][]string{{held}, {`{"type":"progress","bytes":90,"nanos":5}`,
			`{"type":"progress","bytes":120,"nanos":30}`}}, 120, 30},
	} {
		p := newUploadProgress(t.Context(), "id")
		advanced := p.advanced()
		for _, feed := range c.feeds {
			p.read(io.NopCloser(strings.NewReader(`{"type":"ready"}` + "\n" + strings.Join(feed, "\n"))))
		}
		if bytes, nanos := p.counters(); bytes != c.bytes || nanos != c.nanos {
			t.Errorf("%s: receiver pair = (%d, %d), want (%d, %d)", c.name, bytes, nanos, c.bytes, c.nanos)
		}
		select {
		case <-advanced:
		default:
			t.Errorf("%s: an accepted count did not signal waiters", c.name)
		}
		if err := p.awaitReady(t.Context()); err != nil {
			t.Errorf("%s: ready = %v", c.name, err)
		}
	}
}

func TestReattachUploadProgressResumesTheSameAggregate(t *testing.T) {
	t.Parallel()
	const recordsPerFeed = 3
	const window = 1200 * time.Millisecond
	var gets, served atomic.Int64
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		gets.Add(1)
		enc := jsontext.NewEncoder(w)
		_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "ready"})
		w.(http.Flusher).Flush()
		for range recordsPerFeed {
			n := uint64(served.Add(1))
			_ = jsonv2.MarshalEncode(enc, wire.UploadProgress{Type: "progress", Bytes: n, Nanos: n})
			w.(http.Flusher).Flush()
		}
	}))
	defer srv.Close()
	r := &runner{http: srv.Client()}
	ctx, cancel := context.WithTimeout(t.Context(), window)
	defer cancel()
	p := newUploadProgress(ctx, "id")
	defer p.close()
	if err := r.followUploadFeed(ctx, p, srv.URL+"/upload/progress"); err != nil {
		t.Fatal(err)
	}
	<-ctx.Done()
	if carried, _ := p.counters(); carried <= recordsPerFeed {
		t.Errorf("the counter stopped at %d, want it past %d: the reattach resumes the same aggregate",
			carried, recordsPerFeed)
	}
	if paced := int64(window/retryBackoff) + 2; gets.Load() > paced {
		t.Errorf("issued %d progress GETs in %v, want at most %d: the reopen is not paced", gets.Load(), window, paced)
	}
}

func TestUploadProgressPermanentLossFails(t *testing.T) {
	t.Parallel()
	var requests atomic.Int64
	r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
		requests.Add(1)
		header := http.Header{"Graphite-Meter-Auth": {"required"}, "Graphite-Meter-Auth-Url": {"/auth/start"}}
		return &http.Response{StatusCode: http.StatusForbidden, Header: header, Body: http.NoBody, Request: req}, nil
	})}}
	p := newUploadProgress(t.Context(), "id")
	defer p.close()
	p.attach(io.NopCloser(strings.NewReader("")), func(ctx context.Context) (io.ReadCloser, error) {
		return r.openUploadFeed(p.ctx, ctx, "http://progress.invalid/upload/progress")
	})
	<-p.ctx.Done()
	if _, ok := errors.AsType[*AuthRequiredError](context.Cause(p.ctx)); !ok || requests.Load() != 1 {
		t.Fatalf("permanent auth refusal = %v after %d requests, want AuthRequiredError after one",
			context.Cause(p.ctx), requests.Load())
	}
}

type closeRecorder struct {
	io.Reader
	closed atomic.Bool
}

func (c *closeRecorder) Close() error {
	c.closed.Store(true)
	return nil
}

func TestUploadProgressCloseJoinsReadersAndRecovery(t *testing.T) {
	t.Parallel()
	const target = "http://fixture.invalid/upload/progress"
	t.Run("reader", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			var body *closeRecorder
			r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
				feed, writer := io.Pipe()
				go func() {
					_, _ = io.WriteString(writer, "{\"type\":\"ready\"}\n")
					<-req.Context().Done()
					writer.CloseWithError(req.Context().Err())
				}()
				body = &closeRecorder{Reader: feed}
				return &http.Response{StatusCode: http.StatusOK, Body: body, Request: req}, nil
			})}}
			p := newUploadProgress(t.Context(), "id")
			if err := r.followUploadFeed(t.Context(), p, target); err != nil {
				t.Fatal(err)
			}
			if err := p.awaitReady(t.Context()); err != nil {
				t.Fatal(err)
			}
			p.close()
			if !body.closed.Load() {
				t.Fatal("close returned before the reader released its feed")
			}
			late := &closeRecorder{Reader: strings.NewReader("{\"type\":\"progress\",\"bytes\":1,\"nanos\":1}\n")}
			p.attach(late, nil)
			if bytes, _ := p.counters(); !late.closed.Load() || bytes != 0 {
				t.Fatalf("a feed offered after close was adopted: closed=%v bytes=%d", late.closed.Load(), bytes)
			}
		})
	})
	t.Run("recovery", func(t *testing.T) {
		synctest.Test(t, func(t *testing.T) {
			started, release := make(chan struct{}), make(chan struct{})
			var requests int
			var active atomic.Bool
			r := &runner{http: &http.Client{Transport: roundTripFunc(func(req *http.Request) (*http.Response, error) {
				if requests++; requests == 1 {
					body := io.NopCloser(strings.NewReader("{\"type\":\"ready\"}\n"))
					return &http.Response{StatusCode: http.StatusOK, Body: body, Request: req}, nil
				}
				active.Store(true)
				defer active.Store(false)
				close(started)
				<-req.Context().Done()
				<-release
				return nil, req.Context().Err()
			})}}
			p := newUploadProgress(t.Context(), "id")
			if err := r.followUploadFeed(t.Context(), p, target); err != nil {
				t.Fatal(err)
			}
			<-started
			done := make(chan struct{})
			go func() { p.close(); close(done) }()
			synctest.Wait()
			select {
			case <-done:
				t.Fatal("cleanup returned while recovery still owned its request")
			default:
			}
			close(release)
			<-done
			if active.Load() {
				t.Fatal("recovery request was not joined")
			}
		})
	})
}

func TestHandoverWaitsForTheReceiverToSettle(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		name   string
		moving int
		want   time.Duration
	}{
		{"settles", 3, 3 * uploadSettleQuiet},
		{"bounded", 1 << 20, uploadSettleBound},
	} {
		synctest.Test(t, func(t *testing.T) {
			polls := 0
			r := &runner{target: fetchTarget("http://receiver.test"), coordinated: &participantCounters{},
				teardown: t.Context()}
			r.coordinated.upload.Store(newUploadProgress(t.Context(), "id"))
			r.http = &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
				polls++
				body := fmt.Sprintf(`{"bytes":%d,"nanos":%d}`, min(polls, c.moving)*1000, polls)
				return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(body))}, nil
			})}
			start := time.Now()
			r.settleUpload()
			if got := time.Since(start); got != c.want {
				t.Errorf("%s: settled after %v, want %v", c.name, got, c.want)
			}
		})
	}
}

func TestUploadFeedOpenEndsWithItsRecoveryWindow(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) { <-r.Context().Done() }))
	defer srv.Close()
	recovery, cancel := context.WithTimeout(t.Context(), 100*time.Millisecond)
	defer cancel()
	opened := make(chan error, 1)
	go func() {
		_, err := testRunner(srv).openUploadFeed(t.Context(), recovery, srv.URL+"/upload/progress?id=x")
		opened <- err
	}()
	select {
	case err := <-opened:
		if err == nil {
			t.Fatal("a silent feed opened")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("opening a silent feed outlived its recovery window")
	}
}
