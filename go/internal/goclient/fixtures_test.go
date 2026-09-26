package goclient

import (
	"context"
	"encoding/json/v2"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const captureWindow = 300 * time.Millisecond

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

// prepareOne prepares cfg.BaseURL alone, outside any catalogue and without a grant.
func prepareOne(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	return prepare(ctx, cfg, nil, &credential{insecure: cfg.InsecureSkipTLSVerify})
}

func runDirect(ctx context.Context, cfg Config, emit func(Event)) error {
	cfg = cfg.normalized()
	connection, err := prepareOne(ctx, cfg)
	if err != nil {
		emit(Event{Kind: EventDone, At: time.Now(), Err: err})
		return err
	}
	server := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", URL: cfg.BaseURL, Name: "fixture"},
		Connection: connection,
		credential: credential{insecure: cfg.InsecureSkipTLSVerify},
	}
	prepared := &PreparedRun{Servers: []PreparedServer{server}, LatencyFocus: "self"}
	return runSelected(ctx, cfg, prepared, emit)
}

func runSelected(ctx context.Context, cfg Config, prepared *PreparedRun, emit func(Event)) (err error) {
	runSelection(ctx, context.WithoutCancel(ctx), cfg, prepared, func(e Event) {
		if e.Kind == EventDone {
			err = e.Err
		}
		emit(e)
	})
	return err
}

func (r *runner) runTestStage(ctx context.Context, stage Stage, duration time.Duration) error {
	cfg := r.cfg
	cfg.Stages = StageSet{
		Latency:       stage == StageLatency,
		Download:      stage == StageDownload,
		Upload:        stage == StageUpload,
		Bidirectional: stage == StageBidirectional,
	}
	cfg.LatencyDuration, cfg.DownloadDuration, cfg.UploadDuration = duration, duration, duration
	cfg.BidirectionalDuration = duration
	if r.target == nil {
		r.target = fetchTarget(cfg.BaseURL)
	}
	if r.teardown == nil {
		r.teardown = context.WithoutCancel(ctx)
	}
	prepared := PreparedServer{
		Server:     wire.ServerEntry{ID: "self", Name: "fixture", URL: cfg.BaseURL},
		Connection: &PreparedConnection{ThroughputTarget: *r.target, LatencyTarget: r.latencyTarget},
	}
	c := &coordinator{
		cfg:      cfg,
		prepared: &PreparedRun{Servers: []PreparedServer{prepared}, LatencyFocus: "self"},
		servers:  []*participant{{prepared: prepared, transport: r}},
		started:  time.Now(),
		emit:     r.emit,
	}
	err := c.run(ctx)
	r.emit(Event{Kind: EventDone, At: time.Now(), Err: err, Servers: c.details(c.outcome(ctx, err))})
	return err
}

func (r *runner) testTransferResult(ctx context.Context, stage Stage, duration time.Duration) (Result, error) {
	var result Result
	emit := r.emit
	r.emit = func(e Event) {
		if e.Kind == EventResult {
			result = *e.Result
		}
		emit(e)
	}
	defer func() { r.emit = emit }()
	err := r.runTestStage(ctx, stage, duration)
	return result, err
}

func testRunner(srv *httptest.Server) *runner {
	return &runner{
		cfg:           Config{BaseURL: srv.URL}.normalized(),
		http:          srv.Client(),
		target:        fetchTarget(srv.URL),
		latencyTarget: new(testChannel("test-ws", srv.URL, false)),
		streams:       byDirection[int]{down: 1, up: 1},
		emit:          func(Event) {},
	}
}

func testStageGate(start chan struct{}) *stageGate {
	return &stageGate{start: start, reportReady: func() {}, cancel: func(error) {}}
}

func (r *runner) measureNow(ctx context.Context, window time.Duration) (LatencyStats, error) {
	start := make(chan struct{})
	close(start)
	return r.measureLatency(ctx, StageLatency, false, window, testStageGate(start))
}

func fetchTarget(origin string) *wire.ThroughputTarget {
	return new(testTransfer(origin, origin, "http1", false))
}

func testTransfer(id, origin, protocol string, tls bool) wire.ThroughputTarget {
	return wire.ThroughputTarget{
		ID:        id,
		Origin:    origin,
		Transport: wire.TransportFetchStream,
		Protocol:  protocol,
	}
}

func testChannel(id, origin string, tls bool) wire.LatencyTarget {
	return wire.LatencyTarget{
		ID:        id,
		Origin:    origin,
		Transport: wire.TransportWebSocket,
		Protocol:  "http1",
	}
}

func writeProbe(w http.ResponseWriter, _ *http.Request) {
	_ = json.MarshalWrite(w, wire.Probe{
		ClientIP:           "127.0.0.1",
		ClientIPVersion:    4,
		ClientIPSource:     "socket",
		ProtocolNegotiated: "http/1.1",
	})
}

func mountDiscovery(mux *http.ServeMux) {
	mux.HandleFunc("/preflight", func(w http.ResponseWriter, r *http.Request) {
		origin := "http://" + r.Host
		_ = json.MarshalWrite(w, wire.Preflight{
			Server:        wire.ServerInfo{Name: "test"},
			EngineVersion: "test",
			Generation:    "test",
			Capabilities: wire.Capabilities{
				UploadCheckpoint:  true,
				ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1", false)},
				LatencyTargets:    []wire.LatencyTarget{testChannel("ws-http1-clear", origin, false)},
			},
		})
	})
	mux.HandleFunc(route.Servers, func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.SingletonCatalog())
	})
	mux.HandleFunc("/probe", writeProbe)
}

func writeDownload(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/octet-stream")
	_, _ = w.Write(make([]byte, 64*1024))
}

func receiveUpload(received *atomic.Uint64, interrupt func() bool) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if interrupt != nil && interrupt() {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		buf := make([]byte, 32*1024)
		for {
			n, err := r.Body.Read(buf)
			received.Add(uint64(n))
			if err != nil {
				return
			}
			if interrupt != nil && interrupt() {
				panic(http.ErrAbortHandler)
			}
		}
	}
}

// mountUploadReceiver reports reported as the receiver's counters and completes its feed on DELETE.
func mountUploadReceiver(mux *http.ServeMux, reported *atomic.Uint64, upload http.HandlerFunc) {
	mux.HandleFunc("/upload/session", func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, uploadSessionResponse{UploadID: "fixture-upload"})
	})
	mux.HandleFunc("/upload", upload)
	started := time.Now()
	mux.HandleFunc("/upload/checkpoint", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = fmt.Fprintf(w, `{"bytes":%d,"nanos":%d}`, reported.Load(), time.Since(started))
	})
	done := make(chan struct{})
	var once sync.Once
	mux.HandleFunc("/upload/progress", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			once.Do(func() { close(done) })
			w.WriteHeader(http.StatusNoContent)
			return
		}
		w.Header().Set("Content-Type", "application/x-ndjson")
		send := func(kind string) {
			bytes, nanos := reported.Load(), time.Since(started)
			_, _ = fmt.Fprintf(w, "{\"type\":%q,\"bytes\":%d,\"nanos\":%d}\n", kind, bytes, nanos)
			w.(http.Flusher).Flush()
		}
		send("ready")
		ticker := time.Tick(10 * time.Millisecond)
		for {
			select {
			case <-r.Context().Done():
				return
			case <-done:
				send("complete")
				return
			case <-ticker:
				send("progress")
			}
		}
	})
}

func discardUpload(_ http.ResponseWriter, r *http.Request) { _, _ = io.Copy(io.Discard, r.Body) }

func newTransferServer(t *testing.T) *httptest.Server {
	t.Helper()
	var received atomic.Uint64
	mux := http.NewServeMux()
	mountDiscovery(mux)
	mux.HandleFunc("/download", writeDownload)
	mountUploadReceiver(mux, &received, receiveUpload(&received, nil))
	mux.Handle("/ws/ping", pingHandler(answerAll, 0))
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}

func answerAll(uint32) bool  { return true }
func answerNone(uint32) bool { return false }

func pingHandler(answer func(id uint32) bool, delay time.Duration) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := websocket.Accept(w, r, &websocket.AcceptOptions{CompressionMode: websocket.CompressionDisabled})
		if err != nil {
			return
		}
		defer conn.CloseNow()
		ctx := r.Context()
		for {
			_, msg, err := conn.Read(ctx)
			if err != nil {
				return
			}
			id, err := wire.DecodePing(string(msg))
			if err != nil || !answer(id) {
				continue
			}
			pong := []byte(wire.EncodePong(id, 0))
			if delay > 0 {
				time.AfterFunc(delay, func() { _ = conn.Write(ctx, websocket.MessageText, pong) })
			} else if conn.Write(ctx, websocket.MessageText, pong) != nil {
				return
			}
		}
	})
}

// pipedRunner reaches handler over net.Pipe, so one synctest bubble holds both ends on its virtual clock.
func pipedRunner(t *testing.T, handler http.Handler) *runner {
	t.Helper()
	ln := &pipeListener{conns: make(chan net.Conn), closed: make(chan struct{})}
	srv := &http.Server{Handler: handler}
	go func() { _ = srv.Serve(ln) }()
	tr := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		client, server := net.Pipe()
		select {
		case ln.conns <- server:
			return client, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}}
	t.Cleanup(func() {
		tr.CloseIdleConnections()
		_ = srv.Close()
	})
	origin := "http://fixture.invalid"
	return &runner{
		cfg:           Config{BaseURL: origin}.normalized(),
		websocketHTTP: &http.Client{Transport: tr},
		latencyTarget: new(testChannel("test-ws", origin, false)),
		emit:          func(Event) {},
	}
}

type pipeListener struct {
	conns  chan net.Conn
	closed chan struct{}
	once   sync.Once
}

func (l *pipeListener) Accept() (net.Conn, error) {
	select {
	case c := <-l.conns:
		return c, nil
	case <-l.closed:
		return nil, net.ErrClosed
	}
}

func (l *pipeListener) Close() error {
	l.once.Do(func() { close(l.closed) })
	return nil
}

func (l *pipeListener) Addr() net.Addr { return &net.UnixAddr{Name: "pipe", Net: "pipe"} }

func newPingServer(t *testing.T, answer func(id uint32) bool, delay time.Duration) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.Handle("/ws/ping", pingHandler(answer, delay))
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv
}
