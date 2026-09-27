package goclient

import (
	"encoding/json/v2"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestAdaptiveWarmup(t *testing.T) {
	t.Parallel()
	const base = 500 * time.Millisecond
	for _, c := range []struct{ rtt, want time.Duration }{
		{0, base},
		{10 * time.Millisecond, base},
		{100 * time.Millisecond, time.Second},
		{time.Second, 4 * time.Second},
	} {
		if got := adaptiveWarmup(base, c.rtt); got != c.want {
			t.Errorf("adaptiveWarmup(%v, %v) = %v, want %v", base, c.rtt, got, c.want)
		}
	}
}

func TestRunStagesEndToEnd(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		stages     StageSet
		streams    int
		directions []Direction
	}{
		{StageSet{Latency: true}, 1, nil},
		{StageSet{Download: true}, 1, []Direction{Down}},
		{StageSet{Bidirectional: true}, 1, []Direction{Down, Up}},
		{StageSet{Bidirectional: true}, 2, []Direction{Down, Up}},
	} {
		t.Run(fmt.Sprintf("%s/%d", c.stages.name(), c.streams), func(t *testing.T) {
			t.Parallel()
			srv := newTransferServer(t)
			cfg := Config{
				BaseURL:               srv.URL,
				Stages:                c.stages,
				Warmup:                0,
				LatencyDuration:       captureWindow,
				DownloadDuration:      time.Second,
				BidirectionalDuration: time.Second,
				PingInterval:          20 * time.Millisecond,
				LoadedPingInterval:    20 * time.Millisecond,
				TransferStreams:       TransferStreamPolicy{Forced: c.streams},
			}
			var log eventLog
			if err := runDirect(t.Context(), cfg, log.emit); err != nil {
				t.Fatalf("run: %v", err)
			}
			var directions []Direction
			for _, result := range log.results() {
				directions = append(directions, result.Direction)
				if result.Unavailable || result.TotalBytes == 0 || result.Samples == 0 || result.MeanBps <= 0 {
					t.Fatalf("result lacks a measured window: %+v", result)
				}
			}
			events := log.all()
			done := events[len(events)-1]
			if !slices.Equal(directions, c.directions) || done.Kind != EventDone || done.Outcome() != OutcomeComplete {
				t.Fatalf("directions=%v terminal=%+v", directions, done)
			}
			if c.stages.Latency || cfg.LoadedLatency {
				results := done.Servers.Servers[0].Results
				i := slices.IndexFunc(results, func(r Result) bool { return r.Direction == "" })
				if i < 0 || results[i].Latency.Count == 0 {
					t.Fatalf("no latency population: %+v", results)
				}
			}
		})
	}
}

func (s StageSet) name() string {
	var names []string
	for _, stage := range (Config{Stages: s}).Plan() {
		names = append(names, string(stage.Name))
	}
	return strings.Join(names, "+")
}

// An HTTP/2 proxy in front of an HTTP/1.1 backend: the client's own hop decides its protocol and lanes.
func TestRunThroughAnHTTP2ProxyToAnHTTP1Backend(t *testing.T) {
	t.Parallel()
	for _, advertised := range []string{"http2", "negotiated"} {
		t.Run(advertised, func(t *testing.T) {
			t.Parallel()
			var mu sync.Mutex
			var proxyProbe, backendProbe string
			lanes := map[string]map[string]bool{"/download": {}, "/upload": {}}
			mux := http.NewServeMux()
			mux.HandleFunc("/preflight", func(w http.ResponseWriter, _ *http.Request) {
				_ = json.MarshalWrite(w, wire.Preflight{Server: wire.ServerInfo{Name: "proxied"}, EngineVersion: "test",
					Generation: "test", Capabilities: wire.Capabilities{
						UploadCheckpoint: true,
						ThroughputTargets: []wire.ThroughputTarget{
							{Origin: ".", Protocol: advertised, Transport: wire.TransportFetchStream},
						},
						LatencyTargets: []wire.LatencyTarget{{Origin: ".", Transport: wire.TransportWebSocket}},
					}})
			})
			mux.HandleFunc("/probe", func(w http.ResponseWriter, r *http.Request) {
				mu.Lock()
				backendProbe = r.Proto
				mu.Unlock()
				writeProbe(w, r)
			})
			mux.HandleFunc("/download", writeDownload)
			mountUploadReceiver(mux, nil)
			mux.Handle("/ws/ping", pingHandler(answerAll, 0))
			backend := httptest.NewServer(mux)
			defer backend.Close()
			upstream, _ := url.Parse(backend.URL)
			forward := httputil.NewSingleHostReverseProxy(upstream)
			proxy := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				mu.Lock()
				if seen, ok := lanes[r.URL.Path]; ok {
					seen[r.URL.Query().Get("lane")] = true
				}
				if r.URL.Path == "/probe" {
					proxyProbe = r.Proto
				}
				mu.Unlock()
				forward.ServeHTTP(w, r)
			}))
			proxy.EnableHTTP2 = true
			proxy.StartTLS()
			defer proxy.Close()

			cfg := Config{
				BaseURL:               proxy.URL,
				InsecureSkipTLSVerify: true,
				Stages:                StageSet{Bidirectional: true},
				BidirectionalDuration: captureWindow,
				LoadedLatency:         true,
			}.normalized()
			connection, err := prepareOne(t.Context(), cfg)
			if err != nil || connection.ThroughputTarget.Protocol != "http2" ||
				connection.Preflight.Capabilities.ThroughputTargets[0].Protocol != advertised {
				t.Fatalf("prepared %+v, %v; want http2 over the proxy hop, keeping the advertised %q", connection, err,
					advertised)
			}
			if err := runPrepared(t.Context(), cfg, connection, func(Event) {}); err != nil {
				t.Fatalf("run through the proxy: %v", err)
			}
			mu.Lock()
			defer mu.Unlock()
			if proxyProbe != "HTTP/2.0" || backendProbe != "HTTP/1.1" {
				t.Fatalf("probe reached the proxy over %q and the backend over %q", proxyProbe, backendProbe)
			}
			if len(lanes["/download"]) != 1 || len(lanes["/upload"]) != 4 {
				t.Fatalf("opened %d download and %d upload lanes, want HTTP/2's 1 and 4",
					len(lanes["/download"]), len(lanes["/upload"]))
			}
		})
	}
}
