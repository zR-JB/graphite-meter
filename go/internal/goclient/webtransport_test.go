package goclient

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func liveWTSession(t *testing.T) *wtSession { return &wtSession{lifetime: t.Context()} }

func deadWTSession() *wtSession { return &wtSession{} }

func runWTLaneSurfacesAPersistentRedialFailure(t *testing.T) {
	var dials atomic.Int64
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return nil, errors.New("webtransport dial: server at capacity")
		},
	}

	ctx, cancel := context.WithTimeout(t.Context(), 6*time.Second)
	defer cancel()
	started := time.Now()
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		select {
		case <-laneCtx.Done():
			return false, laneCtx.Err()
		case <-time.After(retryBackoff + 50*time.Millisecond):
		}
		return false, errors.New("session closed by the server")
	})

	if err == nil || ctx.Err() != nil || dials.Load() == 0 {
		t.Fatalf("runWTLane = %v after %v and %d redials, want the lost session reported before the stage ends",
			err, time.Since(started), dials.Load())
	}
}

func TestRunWTLaneFastFailureCeiling(t *testing.T) {
	t.Parallel()
	const fastFailures = int64(redialWindow/retryBackoff) + 1
	const slowFailure = retryBackoff + 20*time.Millisecond
	cases := []fastFailureCase{
		{
			name:     "one short of the ceiling is absorbed",
			failures: int(fastFailures) - 1, budget: 3 * time.Second,
			wantErr: false, wantEntries: fastFailures, wantDials: fastFailures,
		},
		{
			name:     "the ceiling reports the failure",
			failures: int(fastFailures), budget: 3 * time.Second,
			wantErr: true, wantEntries: fastFailures, wantDials: fastFailures,
		},
		{
			name:     "a lane that carried bytes before it failed is never reported",
			failures: 4 * int(fastFailures), pause: slowFailure, progress: true, budget: 3 * slowFailure,
			wantErr: false, wantEntries: -1, wantDials: -1,
		},
		{
			name:     "a live session is not re-dialled for one lane's error",
			failures: int(fastFailures), alive: true, budget: 3 * time.Second,
			wantErr: true, wantEntries: fastFailures, wantDials: 0,
		},
		{
			name:     "a live session survives a lane that fails slowly and then runs",
			failures: 2, pause: slowFailure, alive: true, budget: 3 * time.Second,
			wantErr: false, wantEntries: 3, wantDials: 0,
		},
		{
			name:     "a lane failing slowly for the whole window is reported",
			failures: 16, pause: slowFailure, alive: true, budget: 6 * time.Second,
			wantErr: true, wantEntries: -1, wantDials: 0,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) { runFastFailureCase(t, c) })
		})
	}
}

type fastFailureCase struct {
	name        string
	failures    int
	pause       time.Duration
	progress    bool
	alive       bool
	budget      time.Duration
	wantErr     bool
	wantEntries int64
	wantDials   int64
}

func runFastFailureCase(t *testing.T, c fastFailureCase) {
	sess := deadWTSession()
	if c.alive {
		sess = liveWTSession(t)
	}
	var dials, entries atomic.Int64
	host := &wtStageSession{
		sess: sess,
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			if c.alive {
				return liveWTSession(t), nil
			}
			return deadWTSession(), nil
		},
	}
	ctx, cancel := context.WithTimeout(t.Context(), c.budget)
	defer cancel()
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		n := entries.Add(1)
		if c.pause > 0 {
			select {
			case <-laneCtx.Done():
				return false, laneCtx.Err()
			case <-time.After(c.pause):
			}
		}
		if n <= int64(c.failures) {
			return c.progress, errors.New("stream reset")
		}
		<-laneCtx.Done()
		return false, nil
	})

	if (err != nil) != c.wantErr || c.wantErr && ctx.Err() != nil ||
		c.wantEntries >= 0 && entries.Load() != c.wantEntries || c.wantDials >= 0 && dials.Load() != c.wantDials {
		t.Fatalf("runWTLane = %v (stage ended: %v) after %d lane entries and %d redials; want error %v, %d, %d",
			err, ctx.Err() != nil, entries.Load(), dials.Load(), c.wantErr, c.wantEntries, c.wantDials)
	}
}

func TestWebTransportRecoveryInVirtualTime(t *testing.T) {
	t.Parallel()
	for name, test := range map[string]func(*testing.T){
		"persistent redial failure":           runWTLaneSurfacesAPersistentRedialFailure,
		"concurrent redials dedupe":           wtStageSessionDedupesConcurrentRedials,
		"failed establish closes its session": wtStageSessionClosesASessionWhoseEstablishFailed,
		"auth refusal is not retried":         wtStageSessionDoesNotRetryPermanentAuthenticationFailure,
		"cancelled stage is a stop":           runWTLaneReportsACancelledStageAsAStop,
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, test)
		})
	}
}

func wtStageSessionDedupesConcurrentRedials(t *testing.T) {
	var dials atomic.Int64
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return liveWTSession(t), nil
		},
	}

	const lanes = 8
	_, gen := host.current()
	start := make(chan struct{})
	errs := make(chan error, lanes)
	var wg sync.WaitGroup
	for range lanes {
		wg.Go(func() {
			<-start
			errs <- host.redial(t.Context(), gen)
		})
	}
	close(start)
	wg.Wait()
	close(errs)

	for err := range errs {
		if err != nil {
			t.Fatalf("a racing redial reported %v", err)
		}
	}
	if got := dials.Load(); got != 1 {
		t.Errorf("%d lanes on one generation produced %d dials, want 1", lanes, got)
	}
	if _, got := host.current(); got != gen+1 {
		t.Errorf("generation = %d, want %d: the replacement must be published exactly once", got, gen+1)
	}
}

func wtStageSessionClosesASessionWhoseEstablishFailed(t *testing.T) {
	const failures = 2
	var mu sync.Mutex
	var dialed []*wtSession
	var attempts int
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			s := liveWTSession(t)
			mu.Lock()
			dialed = append(dialed, s)
			mu.Unlock()
			return s, nil
		},
		establish: func(context.Context, *wtSession) error {
			mu.Lock()
			attempts++
			n := attempts
			mu.Unlock()
			if n <= failures {
				return errors.New("upload progress stream refused")
			}
			return nil
		},
	}

	started := time.Now()
	if err := host.redial(t.Context(), 0); err != nil {
		t.Fatalf("redial: %v", err)
	}
	elapsed := time.Since(started)

	mu.Lock()
	defer mu.Unlock()
	if len(dialed) != failures+1 {
		t.Fatalf("dialled %d sessions, want %d", len(dialed), failures+1)
	}
	for i, s := range dialed[:failures] {
		if !s.closed.Load() {
			t.Errorf("session %d was left open after its establish failed", i)
		}
	}
	if dialed[failures].closed.Load() {
		t.Error("the adopted session was closed")
	}
	if want := failures * retryBackoff; elapsed < want {
		t.Errorf("%d failed establishes took %v, want at least %v: the retry is not paced", failures, elapsed, want)
	}
}

func wtStageSessionDoesNotRetryPermanentAuthenticationFailure(t *testing.T) {
	var dials atomic.Int64
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return nil, &AuthRequiredError{URL: "https://meter.example/login"}
		},
	}

	err := host.redial(t.Context(), 0)
	if _, ok := errors.AsType[*AuthRequiredError](err); !ok {
		t.Fatalf("redial error = %v, want AuthRequiredError", err)
	}
	if got := dials.Load(); got != 1 {
		t.Fatalf("permanent auth refusal made %d dials, want one without retries", got)
	}
}

func runWTLaneReportsACancelledStageAsAStop(t *testing.T) {
	host := &wtStageSession{
		sess: liveWTSession(t),
		dial: func(context.Context) (*wtSession, error) {
			return nil, errors.New("a cancelled stage must not dial")
		},
	}
	ctx, cancel := context.WithCancel(t.Context())
	entered := make(chan struct{})
	done := make(chan error, 1)
	go func() {
		done <- runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
			close(entered)
			<-laneCtx.Done()
			return false, laneCtx.Err()
		})
	}()

	<-entered
	cancel()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("a cancelled stage reported %v, want a clean stop", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("runWTLane did not return after the stage was cancelled")
	}
}

const wtUnreachableOrigin = "https://127.0.0.1:1"

func TestPrepareReportsTheFetchRefusalWhenWebTransportIsUnreachable(t *testing.T) {
	t.Parallel()
	wt := testTransfer("wt", wtUnreachableOrigin, "http3")
	wt.Transport = wire.TransportWebTransport
	srv := httptest.NewServer(ambiguousFetch(wt))
	defer srv.Close()

	cfg := DefaultConfig()
	cfg.BaseURL, cfg.Stages, cfg.ThroughputTransport = srv.URL, StageSet{Download: true}, "auto"
	_, err := prepareOne(t.Context(), cfg)
	failed, ok := errors.AsType[*PreparationError](err)
	if !ok || !strings.Contains(err.Error(), "select an origin") ||
		len(failed.Preflight.Capabilities.ThroughputTargets) != 3 {
		t.Fatalf("prepare error = %v, want the fetch selection's own refusal keeping all 3 discovered targets", err)
	}
}

func TestWebTransportDialClassifiesAuthenticationRequired(t *testing.T) {
	t.Parallel()
	origin := testOrigin(t, "http3", func(*webtransport.Server) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Set("Graphite-Meter-Auth", "required")
			w.Header().Set("Graphite-Meter-Auth-URL", "https://meter.example/login")
			w.WriteHeader(http.StatusForbidden)
		})
	})
	ctx, cancel := context.WithTimeout(t.Context(), 3*time.Second)
	defer cancel()
	_, err := wtDial(ctx, credential{insecure: true}, origin, "/wt/ping", nil)
	if authErr, ok := errors.AsType[*AuthRequiredError](err); !ok || authErr.URL != "https://meter.example/login" {
		t.Fatalf("wtDial = %v, want the server's authentication challenge", err)
	}
}
