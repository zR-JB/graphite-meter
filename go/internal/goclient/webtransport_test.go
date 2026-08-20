package goclient

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// liveWTSession is a session sentinel that reports itself usable. It stands in
// for a dialled session so the lane state machine can be driven with no server:
// only a QUIC handshake produces the embedded value, and nothing below reads it.
func liveWTSession() *wtSession { return &wtSession{lifetime: context.Background()} }

// deadWTSession is the state a session is in once it is gone: a lane that finds
// one has to have it replaced before it can carry anything.
func deadWTSession() *wtSession { return &wtSession{} }

func webTransportCatalog() wire.Preflight {
	fetch := testTransfer("https://meter:7249", "https://meter:7249", "http3", true)
	wt := testTransfer("https://meter:7249", "https://meter:7249", "http3", true)
	wt.Transport = wire.TransportWebTransport
	ws := testChannel("https://meter:7247", "https://meter:7247", true)
	wtPing := testChannel("https://meter:7249", "https://meter:7249", true)
	wtPing.Transport, wtPing.Protocol = wire.TransportWebTransport, "http3"
	return wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{fetch, wt},
		LatencyTargets:    []wire.LatencyTarget{ws, wtPing},
	}}
}

// TestAutomaticSelectionPreference pins the order: throughput prefers fetch
// streams, latency prefers the datagram bus, whose loss is real packet loss.
func TestAutomaticSelectionPreference(t *testing.T) {
	pf := webTransportCatalog()
	cfg := Config{BaseURL: "https://meter:7249", ThroughputTarget: "auto", ThroughputTransport: "auto", LatencyTarget: "auto", LatencyTransport: "auto"}

	throughput, err := selectTarget(cfg, pf)
	if err != nil || throughput.Transport != wire.TransportFetchStream {
		t.Fatalf("automatic throughput target = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	if err != nil || latency.Transport != wire.TransportWebTransport {
		t.Fatalf("automatic latency target = %+v, %v", latency, err)
	}

	// An origin advertising only WebTransport remains reachable automatically.
	wtOnly := wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{pf.Capabilities.ThroughputTargets[1]}}}
	fallback, err := selectTarget(cfg, wtOnly)
	if err != nil || fallback.Transport != wire.TransportWebTransport {
		t.Fatalf("fallback throughput target = %+v, %v", fallback, err)
	}
}

// TestExplicitTransportSelectionIsHonoured keeps a named transport from silently
// resolving to another one.
func TestExplicitTransportSelectionIsHonoured(t *testing.T) {
	pf := webTransportCatalog()
	cfg := Config{BaseURL: "https://meter:7249", ThroughputTarget: "auto", ThroughputTransport: wire.TransportFetchStream, LatencyTarget: "auto", LatencyTransport: wire.TransportWebSocket}

	throughput, err := selectTarget(cfg, pf)
	if err != nil || throughput.Transport != wire.TransportFetchStream {
		t.Fatalf("explicit throughput transport = %+v, %v", throughput, err)
	}
	latency, err := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	if err != nil || latency.Transport != wire.TransportWebSocket {
		t.Fatalf("explicit latency transport = %+v, %v", latency, err)
	}

	cfg.ThroughputTransport = wire.TransportWebTransport
	cfg.LatencyTransport = wire.TransportWebTransport
	bare := wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{testTransfer("h1", "http://meter:7246", "http1", false)},
		LatencyTargets:    []wire.LatencyTarget{testChannel("h1", "http://meter:7246", false)},
	}}
	if got, err := selectTarget(cfg, bare); err == nil {
		t.Fatalf("throughput fell back to %+v; want a refusal", got)
	}
	if got, err := selectLatencyTarget(cfg, bare.Capabilities.LatencyTargets); err == nil {
		t.Fatalf("latency fell back to %+v; want a refusal", got)
	}
}

func TestConnectionSummaryNamesWebTransport(t *testing.T) {
	if got, want := ConnectionSummary(wire.TransportWebTransport, "http3", true), "WebTransport · HTTP/3 · TLS"; got != want {
		t.Fatalf("ConnectionSummary = %q, want %q", got, want)
	}
}

// TestRunWTLaneSurfacesAPersistentRedialFailure covers the shape a server at
// its session ceiling produces: the stage's session hits the server's session
// duration bound partway through the window, and every replacement is refused
// for the rest of it. The lane stops moving bytes and the measured window's
// clock does not stop with it, so a lane that reports this as a clean stop hands
// the stage a rate that is the truth times the share of the window the session
// was up -- published as a successful measurement.
func TestRunWTLaneSurfacesAPersistentRedialFailure(t *testing.T) {
	t.Parallel()
	var dials atomic.Int64
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return nil, errors.New("webtransport dial: server at capacity")
		},
	}

	// The lane runs longer than one backoff before it fails, so the fast-failure
	// ceiling cannot be what ends it: the redial bound is the only thing left.
	ctx, cancel := context.WithTimeout(context.Background(), 6*time.Second)
	defer cancel()
	started := time.Now()
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		select {
		case <-laneCtx.Done():
			return false, laneCtx.Err()
		case <-time.After(wtRedialBackoff + 50*time.Millisecond):
		}
		return false, errors.New("session closed by the server")
	})

	if err == nil {
		t.Fatalf("runWTLane returned <nil> after %v with %d redial attempts, want the lost session reported: the stage would publish the bytes of the time the session was up over the whole measured window", time.Since(started), dials.Load())
	}
	if ctx.Err() != nil {
		t.Fatalf("the lane only gave up when the stage context expired (%v); the redial is not bounded", err)
	}
	if dials.Load() == 0 {
		t.Error("the lane never tried to replace the session")
	}
}

// TestRunWTLaneReportsALaneThatOnlyEverFailsOnALiveSession covers the shape a
// lane hits when the session stays up and only this lane's streams keep dying:
// the session is never worth replacing, so the redial bound never applies, and
// every failure outlasts the backoff, so the fast-failure ceiling never applies
// either. The lane carries nothing for the whole window while the window's clock
// runs, which is a failed stage rather than a slow one -- reporting it as a stop
// publishes zero bytes over the full window as a successful measurement.
func TestRunWTLaneReportsALaneThatOnlyEverFailsOnALiveSession(t *testing.T) {
	t.Parallel()
	var dials, entries atomic.Int64
	host := &wtStageSession{
		sess: liveWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return liveWTSession(), nil
		},
	}

	// Each failure outlasts wtRedialBackoff, which is what keeps it out of the
	// fast-failure count.
	const failure = wtRedialBackoff + 20*time.Millisecond
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		entries.Add(1)
		select {
		case <-laneCtx.Done():
			return false, laneCtx.Err()
		case <-time.After(failure):
		}
		return false, errors.New("stream reset")
	})

	if err == nil {
		t.Fatalf("runWTLane returned <nil> after %d lane entries against a session that stayed alive throughout: the stage carried no bytes and the window's clock ran the whole time, so this is published as a measurement of zero", entries.Load())
	}
	if ctx.Err() != nil {
		t.Fatalf("the lane only gave up when the stage window ended (%v); the failure is not bounded", err)
	}
	if got := dials.Load(); got != 0 {
		t.Errorf("a live session was re-dialled %d times, want 0: every sibling lane transfers on the session a redial tears down", got)
	}
}

// TestRunWTLaneFastFailureCeiling pins both halves of the retry policy: how many
// back-to-back instant failures a lane absorbs before it reports one, and that a
// lane which carried bytes before it failed resets the bound. The live-session
// rows pin the other half -- one lane's stream error is that lane's to retry,
// and re-dialling the shared session for it stops every sibling lane
// mid-transfer.
func TestRunWTLaneFastFailureCeiling(t *testing.T) {
	t.Parallel()
	// The rows below are written against the constant, so they pin the shape of
	// the policy and not its size. The size is a policy of its own: a lane that
	// gives up on the first instant failure fails a stage on one refused stream,
	// and one that absorbs a long run of them spends the window retrying.
	if wtLaneMaxFastFailures != 5 {
		t.Fatalf("wtLaneMaxFastFailures = %d, want 5", wtLaneMaxFastFailures)
	}
	// slowFailure outlasts one backoff, exercising the time-window half rather
	// than the fast-failure count. Progress is an independent explicit outcome.
	const slowFailure = wtRedialBackoff + 20*time.Millisecond
	cases := []struct {
		name string
		// failures is how many lane entries fail before one blocks until the
		// stage ends.
		failures    int
		pause       time.Duration
		progress    bool
		alive       bool
		budget      time.Duration
		wantErr     bool
		wantEntries int64
		wantDials   int64
	}{
		{
			name:     "one short of the ceiling is absorbed",
			failures: wtLaneMaxFastFailures - 1, budget: 3 * time.Second,
			wantErr: false, wantEntries: int64(wtLaneMaxFastFailures), wantDials: int64(wtLaneMaxFastFailures) - 1,
		},
		{
			name:     "the ceiling reports the failure",
			failures: wtLaneMaxFastFailures, budget: 3 * time.Second,
			wantErr: true, wantEntries: int64(wtLaneMaxFastFailures), wantDials: int64(wtLaneMaxFastFailures) - 1,
		},
		{
			name:     "a lane that carried bytes before it failed is never reported",
			failures: 4 * wtLaneMaxFastFailures, pause: slowFailure, progress: true, budget: 3 * slowFailure,
			wantErr: false, wantEntries: -1, wantDials: -1,
		},
		{
			name:     "a live session is not re-dialled for one lane's error",
			failures: wtLaneMaxFastFailures, alive: true, budget: 3 * time.Second,
			wantErr: true, wantEntries: int64(wtLaneMaxFastFailures), wantDials: 0,
		},
		{
			// The lane fails twice slowly and then runs past wtLaneProgressWindow,
			// which is what "survives" has to mean: a run long enough that the
			// stage was measuring over it. Failing for less than that window is
			// absorbed, and the session is still never re-dialled for it.
			name:     "a live session survives a lane that fails slowly and then runs",
			failures: 2, pause: slowFailure, alive: true, budget: 3 * time.Second,
			wantErr: false, wantEntries: 3, wantDials: 0,
		},
		{
			// The same lane that never gets that run. Each failure outlasts the
			// backoff, so the fast-failure count never sees it, and the session
			// stays alive, so the redial bound never sees it either.
			name:     "a lane failing slowly for the whole window is reported",
			failures: 16, pause: slowFailure, alive: true, budget: 6 * time.Second,
			wantErr: true, wantEntries: -1, wantDials: 0,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			sess := deadWTSession()
			if c.alive {
				sess = liveWTSession()
			}
			var dials, entries atomic.Int64
			host := &wtStageSession{
				sess: sess,
				dial: func(context.Context) (*wtSession, error) {
					dials.Add(1)
					if c.alive {
						return liveWTSession(), nil
					}
					return deadWTSession(), nil
				},
			}
			ctx, cancel := context.WithTimeout(context.Background(), c.budget)
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

			if got := err != nil; got != c.wantErr {
				t.Fatalf("runWTLane err = %v, want an error: %v (after %d lane entries and %d redials)", err, c.wantErr, entries.Load(), dials.Load())
			}
			if c.wantEntries >= 0 && entries.Load() != c.wantEntries {
				t.Errorf("the lane ran %d times, want %d", entries.Load(), c.wantEntries)
			}
			if c.wantDials >= 0 && dials.Load() != c.wantDials {
				t.Errorf("the shared session was re-dialled %d times, want %d: a lane error is not on its own a lost session, and every sibling lane transfers on the session a redial tears down", dials.Load(), c.wantDials)
			}
		})
	}
}

// TestWTStageSessionDedupesConcurrentRedials covers what every lane on a session
// does the moment it dies: they all race here holding the same generation, and
// only the first may dial. The rest adopt what it produced.
func TestWTStageSessionDedupesConcurrentRedials(t *testing.T) {
	var dials atomic.Int64
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return liveWTSession(), nil
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
			errs <- host.redial(context.Background(), gen)
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

// TestWTStageSessionCloseIsFinal keeps a stage that has been torn down from
// opening a fresh session behind itself: the host's close is the end of the
// stage, and a lane still unwinding must not dial past it.
func TestWTStageSessionCloseIsFinal(t *testing.T) {
	sess := deadWTSession()
	var dials atomic.Int64
	host := &wtStageSession{
		sess: sess,
		dial: func(context.Context) (*wtSession, error) {
			dials.Add(1)
			return liveWTSession(), nil
		},
	}

	host.close()
	if !sess.closed.Load() {
		t.Error("closing the stage host left its session open")
	}
	if err := host.redial(context.Background(), 0); err == nil {
		t.Error("a redial after the stage host closed reported success")
	}
	if got := dials.Load(); got != 0 {
		t.Errorf("dialled %d times after the stage host closed, want 0", got)
	}
}

// TestWTStageSessionClosesASessionWhoseEstablishFailed covers the leak a
// half-built session is: it dialled, so it holds a QUIC connection and a transport,
// and the establish step is what rejected it. It has to be released before the
// next attempt, and the next attempt has to be paced.
func TestWTStageSessionClosesASessionWhoseEstablishFailed(t *testing.T) {
	t.Parallel()
	const failures = 2
	var mu sync.Mutex
	var dialed []*wtSession
	var attempts int
	host := &wtStageSession{
		sess: deadWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			s := liveWTSession()
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
	if err := host.redial(context.Background(), 0); err != nil {
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
	if want := failures * wtRedialBackoff; elapsed < want {
		t.Errorf("%d failed establishes took %v, want at least %v: the retry is not paced", failures, elapsed, want)
	}
}

// TestRunWTLaneReportsACancelledStageAsAStop pins the other half of the redial
// contract: a stage the caller cancelled, or one whose own window ended, is a
// clean stop and not a failed measurement.
func TestRunWTLaneReportsACancelledStageAsAStop(t *testing.T) {
	host := &wtStageSession{
		sess: liveWTSession(),
		dial: func(context.Context) (*wtSession, error) {
			return nil, errors.New("a cancelled stage must not dial")
		},
	}
	ctx, cancel := context.WithCancel(context.Background())
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

// wtUnreachableOrigin is a port nothing listens on, so a WebTransport dial to it
// fails the way a UDP-blocked path does, without a server.
const wtUnreachableOrigin = "https://127.0.0.1:1"

// TestPrepareReportsTheFetchRefusalWhenWebTransportIsUnreachable covers what
// automatic selection actually does when several fetch origins are advertised:
// it cannot choose between them, falls through to WebTransport, and a blocked
// UDP path then answers with a QUIC dial error that says nothing about the
// choice the operator has to make. The refusal that is actionable is the fetch
// one, and it is the one that has to reach the caller.
func TestPrepareReportsTheFetchRefusalWhenWebTransportIsUnreachable(t *testing.T) {
	wt := testTransfer("wt", wtUnreachableOrigin, "http3", true)
	wt.Transport = wire.TransportWebTransport
	mux := http.NewServeMux()
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.NewEncoder(w).Encode(wire.Preflight{Capabilities: wire.Capabilities{ThroughputTargets: []wire.ThroughputTarget{
			testTransfer("one", "http://one.example", "negotiated", false),
			testTransfer("two", "http://two.example", "negotiated", false),
			wt,
		}}})
	})
	srv := httptest.NewServer(mux)
	defer srv.Close()

	cfg := DefaultConfig()
	cfg.BaseURL, cfg.Stages, cfg.ThroughputTransport = srv.URL, StageSet{Download: true}, "auto"
	_, err := Prepare(context.Background(), cfg)
	if err == nil {
		t.Fatal("Prepare succeeded with no reachable throughput target")
	}
	if !strings.Contains(err.Error(), "select an origin") {
		t.Fatalf("Prepare error = %q, want the fetch selection's own refusal: automatic selection reached WebTransport only because it could not choose between the advertised fetch origins, and that is what the operator can act on", err)
	}
}

// TestPrepareRejectsAnUnknownTransport keeps a mistyped transport from reading
// as a missing endpoint. "auto target unavailable over webscoket" names the typo
// only to a reader who already knows the spelling.
func TestPrepareRejectsAnUnknownTransport(t *testing.T) {
	cases := []struct {
		name string
		cfg  func(Config) Config
		want string
	}{
		{"throughput", func(c Config) Config { c.ThroughputTransport = "webscoket"; return c }, "invalid throughput transport"},
		{"latency", func(c Config) Config { c.LatencyTransport = "webtransport-datagram"; return c }, "invalid latency transport"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			cfg := c.cfg(DefaultConfig())
			// An unreachable base URL proves the check runs before discovery: a
			// typo is answerable without a server.
			cfg.BaseURL = wtUnreachableOrigin
			_, err := Prepare(context.Background(), cfg)
			if err == nil || !strings.Contains(err.Error(), c.want) {
				t.Fatalf("Prepare error = %v, want one containing %q", err, c.want)
			}
		})
	}
}

// A lane invocation's age is not evidence that it transferred. In particular,
// an accept/open/write can block for a whole progress window and then fail
// without carrying one byte; repeating that schedule must not reset the bound.
func TestRunWTLaneBoundsSlowZeroByteFailures(t *testing.T) {
	t.Parallel()
	host := &wtStageSession{sess: liveWTSession()}
	ctx, cancel := context.WithTimeout(context.Background(), 2*wtLaneProgressWindow+300*time.Millisecond)
	defer cancel()
	entries := 0
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		entries++
		select {
		case <-laneCtx.Done():
			return false, laneCtx.Err()
		case <-time.After(wtLaneProgressWindow + 50*time.Millisecond):
			return false, errors.New("stream failed before carrying a byte")
		}
	})
	if err == nil {
		t.Fatalf("%d consecutive >= progress-window failures returned nil at the stage deadline", entries)
	}
	if ctx.Err() != nil {
		t.Fatalf("zero-byte failures were bounded only by stage cancellation: %v", err)
	}
}

func TestRunWTLaneBoundsMixedZeroByteFailures(t *testing.T) {
	t.Parallel()
	host := &wtStageSession{sess: liveWTSession()}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	entries := 0
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		entries++
		pause := 10 * time.Millisecond
		if entries%2 == 0 {
			pause = wtRedialBackoff + 100*time.Millisecond
		}
		select {
		case <-laneCtx.Done():
			return false, laneCtx.Err()
		case <-time.After(pause):
			return false, errors.New("stream failed before carrying a byte")
		}
	})
	if err == nil || ctx.Err() != nil {
		t.Fatalf("mixed zero-byte failures returned %v after %d entries (ctx=%v)", err, entries, ctx.Err())
	}
}

func TestRunWTLaneRealProgressResetsFailureBounds(t *testing.T) {
	host := &wtStageSession{sess: liveWTSession()}
	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()
	entries := 0
	err := runWTLane(ctx, host, func(laneCtx context.Context, _ *wtSession) (bool, error) {
		entries++
		if entries <= 2*wtLaneMaxFastFailures {
			return true, errors.New("stream reset after carrying bytes")
		}
		<-laneCtx.Done()
		return false, laneCtx.Err()
	})
	if err != nil {
		t.Fatalf("progressing lane reported %v after %d entries", err, entries)
	}
	if entries != 2*wtLaneMaxFastFailures+1 {
		t.Fatalf("lane entered %d times, want %d", entries, 2*wtLaneMaxFastFailures+1)
	}
}
