package goclient

import (
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/endpoint"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const captureWindow = 300 * time.Millisecond

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func prepareOne(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	return prepare(ctx, cfg, nil, &credential{insecure: cfg.InsecureSkipTLSVerify})
}

func runDirect(ctx context.Context, cfg Config, emit func(Event)) error {
	connection, err := prepareOne(ctx, cfg.normalized())
	if err != nil {
		return err
	}
	return runPrepared(ctx, cfg, connection, emit)
}

func runPrepared(ctx context.Context, cfg Config, connection *PreparedConnection, emit func(Event)) error {
	cfg = cfg.normalized()
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

func testStageGate(start chan struct{}) *stageGate {
	return &stageGate{start: start, reportReady: func() {}, cancel: func(error) {}}
}

func (r *runner) measureNow(ctx context.Context, underLoad bool, window time.Duration) (LatencyStats, error) {
	start := make(chan struct{})
	close(start)
	stage := StageLatency
	if underLoad {
		stage = StageDownload
	}
	return r.measureLatency(ctx, stage, underLoad, window, testStageGate(start))
}

func fetchTarget(origin string) *wire.ThroughputTarget {
	return new(testTransfer(origin, origin, "http1"))
}

func testTransfer(id, origin, protocol string) wire.ThroughputTarget {
	return wire.ThroughputTarget{
		ID:        id,
		Origin:    origin,
		Transport: wire.TransportFetchStream,
		Protocol:  protocol,
	}
}

func testChannel(id, origin string) wire.LatencyTarget {
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
				ThroughputTargets: []wire.ThroughputTarget{testTransfer("http1-clear", origin, "http1")},
				LatencyTargets:    []wire.LatencyTarget{testChannel("ws-http1-clear", origin)},
			},
		})
	})
	mux.HandleFunc(route.Servers, func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.SingletonCatalog())
	})
	mux.HandleFunc("/probe", writeProbe)
}

// pacedDownload lets virtual time pass between writes over a pipe.
func pacedDownload(w http.ResponseWriter, _ *http.Request) {
	time.Sleep(time.Millisecond)
	_, _ = w.Write(make([]byte, 32*1024))
}

func writeDownload(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/octet-stream")
	_, _ = w.Write(make([]byte, 64*1024))
}

// mountUploadReceiver serves the server's own receiver; paced bodies let virtual time advance, and a fault
// refuses new lanes and drops running ones.
func mountUploadReceiver(mux *http.ServeMux, fault func() bool) {
	upload := endpoint.NewUpload(nil, nil)
	mux.HandleFunc(route.UploadSession, upload.ServeSession)
	mux.HandleFunc(route.UploadProgress, upload.ServeProgress)
	mux.HandleFunc(route.UploadCheckpoint, upload.ServeCheckpoint)
	lanes := upload.Handler(wire.IdleBound)
	mux.HandleFunc(route.Upload, func(w http.ResponseWriter, r *http.Request) {
		if fault != nil && fault() {
			http.Error(w, "fixture dropout", http.StatusGone)
			return
		}
		r.Body = pacedBody{r.Body, r.Context(), fault}
		lanes.ServeHTTP(w, r)
	})
}

type pacedBody struct {
	io.ReadCloser
	ctx   context.Context
	fault func() bool
}

func (b pacedBody) Read(p []byte) (int, error) {
	if b.fault != nil && b.fault() {
		return 0, errors.New("fixture disconnected")
	}
	timer := time.NewTimer(time.Millisecond)
	defer timer.Stop()
	select {
	case <-b.ctx.Done():
		return 0, b.ctx.Err()
	case <-timer.C:
	}
	return b.ReadCloser.Read(p[:min(len(p), 8192)])
}

// mountSilentReceiver accepts upload bytes but reports none, while receiver time advances.
func mountSilentReceiver(mux *http.ServeMux) {
	mux.HandleFunc(route.UploadSession, func(w http.ResponseWriter, _ *http.Request) {
		_ = json.MarshalWrite(w, wire.UploadSession{UploadID: "silent"})
	})
	mux.HandleFunc(route.Upload, func(_ http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, pacedBody{r.Body, r.Context(), nil})
	})
	started := time.Now()
	mux.HandleFunc(route.UploadCheckpoint, func(w http.ResponseWriter, _ *http.Request) {
		_, _ = fmt.Fprintf(w, `{"bytes":0,"nanos":%d}`, time.Since(started)+1)
	})
	mux.HandleFunc(route.UploadProgress, func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodDelete {
			return
		}
		_, _ = fmt.Fprintln(w, `{"type":"ready"}`)
		for ctx := r.Context(); ctx.Err() == nil; time.Sleep(10 * time.Millisecond) {
			_, _ = fmt.Fprintf(w, "{\"type\":\"progress\",\"bytes\":0,\"nanos\":%d}\n", time.Since(started)+1)
			w.(http.Flusher).Flush()
		}
	})
}

func newTransferServer(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mountDiscovery(mux)
	mux.HandleFunc("/download", writeDownload)
	mountUploadReceiver(mux, nil)
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
	var conns sync.WaitGroup
	srv := &http.Server{Handler: handler, ConnState: func(_ net.Conn, state http.ConnState) {
		switch state {
		case http.StateNew:
			conns.Add(1)
		case http.StateClosed, http.StateHijacked:
			conns.Done()
		}
	}}
	go func() { _ = srv.Serve(ln) }()
	tr := &http.Transport{DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
		client, server := net.Pipe()
		select {
		case ln.conns <- loopbackConn{server}:
			return client, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}}
	t.Cleanup(func() {
		tr.CloseIdleConnections()
		_ = srv.Close()
		conns.Wait() // Time stops when the bubble's test returns, so a sleeping connection must finish first.
	})
	origin := "http://fixture.invalid"
	client := &http.Client{Transport: tr}
	return &runner{
		cfg:           Config{BaseURL: origin}.normalized(),
		http:          client,
		websocketHTTP: client,
		target:        fetchTarget(origin),
		latencyTarget: new(testChannel("test-ws", origin)),
		streams:       byDirection[int]{down: 1, up: 1},
		emit:          func(Event) {},
	}
}

// loopbackConn gives a pipe the loopback address the upload receiver keys its owner by.
type loopbackConn struct{ net.Conn }

func (loopbackConn) RemoteAddr() net.Addr { return &net.TCPAddr{IP: net.IPv6loopback, Port: 1} }

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

type eventLog struct {
	mu     sync.Mutex
	events []Event
}

func (l *eventLog) emit(e Event) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, e)
}

func (l *eventLog) all() []Event {
	l.mu.Lock()
	defer l.mu.Unlock()
	return slices.Clone(l.events)
}

func (l *eventLog) phases() (phases []Phase) {
	for _, e := range l.all() {
		if e.Kind == EventStage {
			phases = append(phases, e.Phase)
		}
	}
	return phases
}

// results are the aggregate results, without any server's own.
func (l *eventLog) results() (results []Result) {
	for _, e := range l.all() {
		if e.Kind == EventResult && e.ServerID == "" {
			results = append(results, *e.Result)
		}
	}
	return results
}

func (l *eventLog) details() (details *RunDetails) {
	for _, e := range l.all() {
		if e.Servers != nil {
			details = e.Servers
		}
	}
	return details
}
